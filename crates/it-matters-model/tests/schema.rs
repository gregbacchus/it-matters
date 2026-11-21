//! `docs/design/plugin-protocol.schema.json` against the real parser, and the JSON in the documentation
//! against both.
//!
//! The schema describes *structure*. For every message in a labelled corpus the schema, the parser
//! (`decode_line`, plus `DeviceSpec::validate` for `register`, the checks the host makes) and the label must all
//! agree; the places where a schema cannot express the host's rules are listed in one test, so the
//! documentation of those limits stays true.

use it_matters_model::protocol::{HostMessage, PluginMessage, decode_line};
use serde_json::{Value, json};

const SCHEMA: &str = include_str!("../../../docs/design/plugin-protocol.schema.json");
const DOC: &str = include_str!("../../../docs/design/plugin-protocol.md");

fn validator(def: &str) -> jsonschema::Validator {
    let mut root: Value = serde_json::from_str(SCHEMA).expect("the schema is valid JSON");
    root["$ref"] = json!(format!("#/$defs/{def}"));
    jsonschema::validator_for(&root).expect("the schema is a valid JSON Schema")
}

fn plugin_schema(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|v| validator("PluginMessage").is_valid(&v))
}

fn host_schema(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|v| validator("HostMessage").is_valid(&v))
}

/// What the host's parser says about a message from a plugin (including the device checks on `register`).
fn plugin_parser(line: &str) -> bool {
    match decode_line::<PluginMessage>(line) {
        Ok(PluginMessage::Register { device }) => device.validate().is_ok(),
        Ok(_) => true,
        Err(_) => false,
    }
}

fn host_parser(line: &str) -> bool {
    decode_line::<HostMessage>(line).is_ok()
}

fn speaker() -> String {
    concat!(
        r#"{"type":"register","device":{"id":"audio","name":"Audio","capabilities":["#,
        r#"{"id":"volume","type":"level","min":0,"max":100,"writable":true},"#,
        r#"{"id":"mute","type":"boolean","writable":true,"role":"mute"}]}}"#
    )
    .to_owned()
}

#[test]
fn the_schema_and_the_parser_agree_on_messages_from_plugins() {
    let speaker = speaker();
    let valid = [
        r#"{"type":"hello","plugin":"p","version":"1","proto":1}"#,
        r#"{"type":"hello","plugin":"p","version":"1","proto":1,"proof":"ab12"}"#,
        r#"{"type":"hello","plugin":"p","version":"1","proto":1,"proof":null}"#,
        r#"{"type":"hello","plugin":"p","version":"1","proto":1,"somethingNew":true}"#,
        speaker.as_str(),
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"online","type":"boolean"}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"o","type":"boolean","role":null}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"o","type":"boolean","role":"occupancy"}]}}"#,
        r#"{"type":"unregister","device":"pc"}"#,
        r#"{"type":"update","device":"pc","capability":"online","value":true}"#,
        r#"{"type":"update","device":"pc","capability":"volume","value":-5}"#,
        r#"{"type":"set_result","req_id":7,"ok":true}"#,
        r#"{"type":"set_result","req_id":7,"ok":false,"error":"no such sink"}"#,
        r#"{"type":"set_result","req_id":0,"ok":false,"error":null}"#,
        r#"{"type":"ping"}"#,
        r#"{"type":"pong"}"#,
        r#"{"type":"ping","extra":1}"#,
    ];
    let invalid = [
        r#"{"type":"explode"}"#,
        r#"{"plugin":"p"}"#,
        r#"[]"#,
        r#""hello""#,
        r#"{"type":"hello","plugin":"p","version":"1"}"#,
        r#"{"type":"hello","plugin":"p","version":"1","proto":"1"}"#,
        r#"{"type":"hello","plugin":"p","version":"1","proto":-1}"#,
        r#"{"type":"hello","plugin":"p","version":"1","proto":4294967296}"#,
        r#"{"type":"hello","plugin":"p","version":1,"proto":1}"#,
        r#"{"type":"register"}"#,
        r#"{"type":"register","device":{"name":"PC","capabilities":[{"id":"o","type":"boolean"}]}}"#,
        r#"{"type":"register","device":{"id":"PC","name":"PC","capabilities":[{"id":"o","type":"boolean"}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"   ","capabilities":[{"id":"o","type":"boolean"}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC"}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"Bad Id","type":"boolean"}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"o","type":"colour"}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"v","type":"level","max":9}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"v","type":"level","min":0}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"o","type":"boolean","role":"king"}]}}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"o","type":"boolean","writable":"yes"}]}}"#,
        r#"{"type":"unregister"}"#,
        r#"{"type":"unregister","device":5}"#,
        r#"{"type":"update","device":"pc","capability":"c"}"#,
        r#"{"type":"update","device":"pc","capability":"c","value":"loud"}"#,
        r#"{"type":"update","device":"pc","capability":"c","value":null}"#,
        r#"{"type":"update","device":"pc","capability":"c","value":0.5}"#,
        r#"{"type":"set_result","req_id":1}"#,
        r#"{"type":"set_result","req_id":-1,"ok":true}"#,
        r#"{"type":"set_result","req_id":"1","ok":true}"#,
        r#"{"type":"set_result","req_id":1,"ok":true,"error":5}"#,
        // A host-only message is not a valid plugin message.
        r#"{"type":"welcome","proto":1}"#,
        r#"{"type":"set","req_id":1,"device":"d","capability":"c","value":true}"#,
        r#"{"type":"challenge","nonce":"00"}"#,
    ];
    for line in valid {
        assert!(plugin_parser(line), "the parser rejects what should be valid: {line}");
        assert!(plugin_schema(line), "the schema rejects what should be valid: {line}");
    }
    for line in invalid {
        assert!(!plugin_parser(line), "the parser accepts what should be invalid: {line}");
        assert!(!plugin_schema(line), "the schema accepts what should be invalid: {line}");
    }
}

#[test]
fn the_schema_and_the_parser_agree_on_messages_from_the_host() {
    let valid = [
        r#"{"type":"challenge","nonce":"00112233445566778899aabbccddeeff"}"#,
        r#"{"type":"welcome","proto":1}"#,
        r#"{"type":"set","req_id":7,"device":"d","capability":"c","value":true}"#,
        r#"{"type":"set","req_id":7,"device":"d","capability":"c","value":100}"#,
        r#"{"type":"error","message":"authentication failed"}"#,
        r#"{"type":"ping"}"#,
        r#"{"type":"pong"}"#,
        r#"{"type":"welcome","proto":1,"somethingNew":[1,2]}"#,
    ];
    let invalid = [
        r#"{"type":"explode"}"#,
        r#"{"type":"challenge"}"#,
        r#"{"type":"challenge","nonce":5}"#,
        r#"{"type":"welcome","proto":"1"}"#,
        r#"{"type":"welcome"}"#,
        r#"{"type":"set","device":"d","capability":"c","value":true}"#,
        r#"{"type":"set","req_id":1,"device":"d","capability":"c","value":1.5}"#,
        r#"{"type":"set","req_id":-1,"device":"d","capability":"c","value":true}"#,
        r#"{"type":"set","req_id":1,"device":"d","capability":"c"}"#,
        r#"{"type":"error","message":5}"#,
        r#"{"type":"error"}"#,
        // A plugin-only message is not a valid host message.
        r#"{"type":"hello","plugin":"p","version":"1","proto":1}"#,
        r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"o","type":"boolean"}]}}"#,
        r#"{"type":"set_result","req_id":1,"ok":true}"#,
    ];
    for line in valid {
        assert!(host_parser(line), "the parser rejects what should be valid: {line}");
        assert!(host_schema(line), "the schema rejects what should be valid: {line}");
    }
    for line in invalid {
        assert!(!host_parser(line), "the parser accepts what should be invalid: {line}");
        assert!(!host_schema(line), "the schema accepts what should be invalid: {line}");
    }
}

/// What the schema cannot say. These are listed in the schema's description and in section 13 of the
/// reference: if one of them stops diverging, update both places.
#[test]
fn the_documented_limits_of_the_schema_are_real() {
    // A whole number written with a decimal point is an integer to JSON Schema, but the host rejects it.
    let whole_float = r#"{"type":"update","device":"pc","capability":"v","value":30.0}"#;
    assert!(plugin_schema(whole_float) && !plugin_parser(whole_float));
    // Two capabilities with the same id.
    let duplicate = r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"a","type":"boolean"},{"id":"a","type":"boolean"}]}}"#;
    assert!(plugin_schema(duplicate) && !plugin_parser(duplicate));
    // A level whose min is not below its max.
    let inverted = r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"v","type":"level","min":5,"max":5}]}}"#;
    assert!(plugin_schema(inverted) && !plugin_parser(inverted));
    // Mapping rules (a level needs a mute, ...) are not structural either.
    let lonely_level = r#"{"type":"register","device":{"id":"pc","name":"PC","capabilities":[{"id":"v","type":"level","min":0,"max":9,"writable":true}]}}"#;
    assert!(plugin_schema(lonely_level), "mapping is checked by the host, not by the schema");
}

// ---- the examples in the documentation ----

/// The JSON of every example line in the documentation's code blocks: `>` lines (plugin to host) and `<`
/// lines (host to plugin).
fn transcript_lines(prefix: &str) -> Vec<String> {
    DOC.lines().filter_map(|l| l.strip_prefix(prefix)).filter(|l| l.starts_with('{')).map(str::to_owned).collect()
}

#[test]
fn every_example_session_line_in_the_reference_is_valid() {
    let to_host = transcript_lines("> ");
    let to_plugin = transcript_lines("< ");
    assert!(to_host.len() >= 10 && to_plugin.len() >= 6, "the documentation should contain example sessions");
    for line in &to_host {
        assert!(plugin_parser(line), "the parser rejects a documented plugin message: {line}");
        assert!(plugin_schema(line), "the schema rejects a documented plugin message: {line}");
    }
    for line in &to_plugin {
        assert!(host_parser(line), "the parser rejects a documented host message: {line}");
        assert!(host_schema(line), "the schema rejects a documented host message: {line}");
    }
}

#[test]
fn every_inline_message_example_in_the_reference_is_valid_json_and_a_known_message() {
    let mut checked = 0;
    for segment in DOC.split('`').skip(1).step_by(2).filter(|s| s.starts_with("{\"type\":")) {
        let value: Value = serde_json::from_str(segment).unwrap_or_else(|e| panic!("not valid JSON ({e}): {segment}"));
        let known = validator("PluginMessage").is_valid(&value) || validator("HostMessage").is_valid(&value);
        assert!(known, "the schema knows neither direction for the documented message: {segment}");
        checked += 1;
    }
    assert!(checked >= 15, "expected many inline examples, found {checked}");
}

#[test]
fn the_schema_has_a_definition_for_every_message_type_in_the_code() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let titles = |def: &str| -> Vec<String> {
        schema["$defs"][def]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["title"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(titles("PluginMessage"), ["hello", "register", "unregister", "update", "set_result", "ping", "pong"]);
    assert_eq!(titles("HostMessage"), ["challenge", "welcome", "set", "error", "ping", "pong"]);
}
