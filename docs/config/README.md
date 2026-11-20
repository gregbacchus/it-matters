# Configuration overview

it-matters reads one YAML file. A copy-paste starting point with every option set is
[`examples/config.yaml`](../../examples/config.yaml); check a file with `it-matters check`.

| Location | Used when |
|---|---|
| `--config <file>` | always, if given (a missing file is an error) |
| `$XDG_CONFIG_HOME/it-matters/config.yaml` (or `~/.config/it-matters/config.yaml`) | otherwise, if it exists |
| `/etc/it-matters/config.yaml` | otherwise, if it exists (the system-service location) |

**Every option is optional.** A missing file, an empty file, or a missing key falls back to the
defaults listed on these pages. Unknown keys are ignored (so old and new files keep loading), but
values with the wrong type, and the semantic checks listed with each option, are errors: the daemon
refuses to start and `it-matters check` prints every problem.

## Sections

| Key | Page |
|---|---|
| `watch` | [General](general.md) |
| `bridge` | [General](general.md) |
| `matter` | [Matter](matter.md) |
| `ipc` | [Plugins](plugins.md) |
| `plugins` | [Plugins](plugins.md) |
| `devices` | [Plugins](plugins.md) |

"Applies" in the tables says when a change takes effect: **live** (on file change) or **startup
only** (needs a restart). Reload is not wired up yet, so everything is currently *startup only*.
