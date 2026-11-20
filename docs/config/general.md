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
| `bridge.name` | string | `it-matters` | startup only | The bridge's name in controllers. Must not be empty. |
| `bridge.vendor_id` | integer (0-65535) | `0xFFF1` | startup only | Matter vendor id. `0xFFF1` is the test vendor, fine for home use. |
| `bridge.product_id` | integer (0-65535) | `0x8000` | startup only | Matter product id. |
