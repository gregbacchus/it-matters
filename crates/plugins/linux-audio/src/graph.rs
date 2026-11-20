//! Reading the PipeWire graph (`pw-dump`) and `wpctl` output. Pure parsing, tested with captured samples.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// What the plugin needs from `pw-dump`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Graph {
    /// `(id, node.name, media.class)` of every node.
    pub nodes: Vec<(u32, String, String)>,
    /// A running capture stream is linked from a microphone-type source.
    pub mic_active: bool,
}

impl Graph {
    /// The id of the node with this `node.name`.
    pub fn node_id(&self, name: &str) -> Option<u32> {
        self.nodes.iter().find(|(_, n, _)| n == name).map(|(id, _, _)| *id)
    }
}

/// Parses the JSON that `pw-dump` prints.
///
/// **Microphone in use** means: a node of class `Stream/Input/Audio` is `running` and has a link
/// (not in error) *from* a node whose class starts with `Audio/Source`. Capturing a speaker's monitor
/// (a screen recorder recording desktop audio) is linked from an `Audio/Sink` node and does not count.
pub fn parse_pw_dump(json: &str) -> Result<Graph, String> {
    let objects: Vec<Value> = serde_json::from_str(json).map_err(|e| format!("cannot parse pw-dump output: {e}"))?;
    let mut nodes = Vec::new();
    let mut class_of: HashMap<u32, String> = HashMap::new();
    let mut running_captures: HashSet<u32> = HashSet::new();
    for o in &objects {
        if o["type"] != "PipeWire:Interface:Node" {
            continue;
        }
        let Some(id) = o["id"].as_u64().map(|i| i as u32) else { continue };
        let props = &o["info"]["props"];
        let class = props["media.class"].as_str().unwrap_or("").to_owned();
        let name = props["node.name"].as_str().unwrap_or("").to_owned();
        if class == "Stream/Input/Audio" && o["info"]["state"] == "running" {
            running_captures.insert(id);
        }
        class_of.insert(id, class.clone());
        nodes.push((id, name, class));
    }
    let mut mic_active = false;
    for o in &objects {
        if o["type"] != "PipeWire:Interface:Link" {
            continue;
        }
        let info = &o["info"];
        let (Some(from), Some(to)) = (info["output-node-id"].as_u64(), info["input-node-id"].as_u64()) else {
            continue;
        };
        let broken = matches!(info["state"].as_str(), Some("error" | "unlinked"));
        let from_source = class_of.get(&(from as u32)).is_some_and(|c| c.starts_with("Audio/Source"));
        if !broken && from_source && running_captures.contains(&(to as u32)) {
            mic_active = true;
        }
    }
    Ok(Graph { nodes, mic_active })
}

/// Parses `wpctl get-volume`: `Volume: 0.58` or `Volume: 0.58 [MUTED]`. Returns `(volume, muted)` where
/// the volume is `wpctl`'s linear value (1.0 = 100 %).
pub fn parse_volume(text: &str) -> Result<(f64, bool), String> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("Volume:"))
        .ok_or_else(|| format!("unexpected wpctl output: {:?}", text.trim()))?;
    let value = line
        .trim_start()
        .trim_start_matches("Volume:")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| format!("cannot read a volume from {:?}", line.trim()))?;
    Ok((value, line.contains("[MUTED]")))
}

/// A volume as a whole percent, clamped to 0..=100 (PipeWire allows more than 100 %).
pub fn to_percent(volume: f64) -> u8 {
    (volume * 100.0).round().clamp(0.0, 100.0) as u8
}

/// A percent as the argument for `wpctl set-volume`.
pub fn percent_arg(percent: u8) -> String {
    format!("{:.2}", f64::from(percent.min(100)) / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, name: &str, class: &str, state: &str) -> String {
        format!(
            r#"{{"id":{id},"type":"PipeWire:Interface:Node","info":{{"state":"{state}","props":{{"media.class":"{class}","node.name":"{name}"}}}}}}"#
        )
    }
    fn link(from: u32, to: u32, state: &str) -> String {
        format!(
            r#"{{"id":900,"type":"PipeWire:Interface:Link","info":{{"output-node-id":{from},"input-node-id":{to},"state":"{state}"}}}}"#
        )
    }
    fn dump(parts: &[String]) -> String {
        format!("[{}]", parts.join(","))
    }

    #[test]
    fn volume_output_parses() {
        assert_eq!(parse_volume("Volume: 0.58\n"), Ok((0.58, false)));
        assert_eq!(parse_volume("Volume: 0.30 [MUTED]\n"), Ok((0.30, true)));
        assert_eq!(parse_volume("Volume: 1.50"), Ok((1.5, false)));
        assert_eq!(parse_volume("Volume: 0.00\n"), Ok((0.0, false)));
    }

    #[test]
    fn unexpected_volume_output_is_an_error() {
        assert!(parse_volume("").is_err());
        assert!(parse_volume("Error: 'x' is not a valid number").is_err());
        assert!(parse_volume("Volume: loud").is_err());
        assert!(parse_volume("Volume: 0,58").is_err(), "a comma decimal means the locale was not forced");
    }

    #[test]
    fn percent_is_rounded_and_clamped() {
        assert_eq!([0.0, 0.004, 0.005, 0.58, 1.0, 1.5, 9.0].map(to_percent), [0, 0, 1, 58, 100, 100, 100]);
        assert_eq!(to_percent(0.295), 30);
    }

    #[test]
    fn every_percent_survives_the_wpctl_round_trip() {
        for p in 0..=100u8 {
            let arg = percent_arg(p);
            let back = to_percent(arg.parse::<f64>().unwrap());
            assert_eq!(back, p, "{p}% -> {arg}");
        }
        assert_eq!(percent_arg(30), "0.30");
        assert_eq!(percent_arg(200), "1.00", "out-of-range input is clamped");
    }

    #[test]
    fn a_microphone_capture_means_the_mic_is_active() {
        let g = parse_pw_dump(&dump(&[
            node(60, "sink", "Audio/Sink", "suspended"),
            node(61, "mic", "Audio/Source", "running"),
            node(76, "app", "Stream/Input/Audio", "running"),
            link(61, 76, "active"),
        ]))
        .unwrap();
        assert!(g.mic_active);
        assert_eq!(g.node_id("sink"), Some(60));
        assert_eq!(g.node_id("nope"), None);
    }

    #[test]
    fn capturing_desktop_audio_does_not_count() {
        let g = parse_pw_dump(&dump(&[
            node(60, "sink", "Audio/Sink", "running"),
            node(61, "mic", "Audio/Source", "running"),
            node(76, "recorder", "Stream/Input/Audio", "running"),
            link(60, 76, "active"),
        ]))
        .unwrap();
        assert!(!g.mic_active, "a screen recorder capturing the speakers is not a microphone");
    }

    #[test]
    fn idle_or_broken_captures_do_not_count() {
        let base = |state: &str, link_state: &str| {
            parse_pw_dump(&dump(&[
                node(61, "mic", "Audio/Source", "running"),
                node(76, "app", "Stream/Input/Audio", state),
                link(61, 76, link_state),
            ]))
            .unwrap()
            .mic_active
        };
        assert!(base("running", "active"));
        assert!(!base("idle", "active"), "a paused stream");
        assert!(!base("suspended", "active"));
        assert!(!base("running", "error"));
        assert!(!base("running", "unlinked"));
    }

    #[test]
    fn a_source_with_no_capture_stream_is_not_in_use() {
        let g = parse_pw_dump(&dump(&[node(61, "mic", "Audio/Source", "running")])).unwrap();
        assert!(!g.mic_active, "a running source alone (for example a level meter in a mixer) is not an app capturing");
    }

    #[test]
    fn virtual_sources_count_as_microphones() {
        let g = parse_pw_dump(&dump(&[
            node(70, "echo-cancel-source", "Audio/Source/Virtual", "running"),
            node(76, "app", "Stream/Input/Audio", "running"),
            link(70, 76, "active"),
        ]))
        .unwrap();
        assert!(g.mic_active);
    }

    #[test]
    fn unrelated_objects_and_garbage_are_handled() {
        assert!(parse_pw_dump("[]").unwrap() == Graph::default());
        assert!(parse_pw_dump(r#"[{"type":"PipeWire:Interface:Device","id":1},{"id":"x"},42]"#).is_ok());
        assert!(parse_pw_dump("not json").is_err());
        assert!(parse_pw_dump("{}").is_err(), "pw-dump prints an array");
    }
}
