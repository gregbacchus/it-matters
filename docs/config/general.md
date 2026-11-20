# General

```yaml
watch: true
bridge:
  name: it-matters
  vendor_id: 0xFFF1
  product_id: 0x8000
```

| Option | Type | Default | Applies | Description |
|---|---|---|---|---|
| `watch` | bool | `true` | startup only | Reload the file when it changes (reload is not implemented yet; the option is reserved). |
| `bridge` | map | see below | startup only | How the bridge presents itself to controllers. |
| `bridge.name` | string | `it-matters` | startup only | The bridge's name in controllers (the Matter node's device name). Must not be empty and at most 32 characters (a Matter limit); `it-matters check` reports a longer one. |
| `bridge.vendor_id` | integer (0-65535) | `0xFFF1` | startup only | Matter vendor id advertised to controllers. `0xFFF1` is the Matter *test* vendor id, fine for home use. The bridge authenticates itself with the test device certificates built into rs-matter, which belong to vendor `0xFFF1`: **keep the default unless you know your controller accepts another value** (only the default has been verified). |
| `bridge.product_id` | integer (0-65535) | `0x8000` | startup only | Matter product id advertised to controllers (`0x8000` to `0x8FFF` are the test product ids). |

## Notes

* `watch` is accepted and documented for forward compatibility, but the daemon does not reload its
  configuration yet: **restart `it-matters` after editing the file**, and check it first with
  `it-matters check`.
* The bridge identifies itself with test attestation certificates, so controllers treat it as an
  *uncertified* device. Some ecosystems warn about this when pairing and some may refuse it; see
  [what has been verified](../matter.md#what-has-been-verified).
