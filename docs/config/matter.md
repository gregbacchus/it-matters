# Matter

```yaml
matter:
  port: 5540
  storage_dir: /var/lib/it-matters
  passcode: null
  discriminator: null
```

| Option | Type | Default | Applies | Description |
|---|---|---|---|---|
| `matter` | map | see below | startup only | The Matter side of the bridge. |
| `matter.port` | integer (0-65535) | `5540` | startup only | UDP port the Matter stack listens on. |
| `matter.storage_dir` | path | `/var/lib/it-matters` | startup only | Where fabrics, commissioning data and endpoint ids are persisted. Must be writable by the daemon. |
| `matter.passcode` | integer or `null` | `null` | startup only | Setup passcode (1 to 99999998; trivial values such as `11111111` and `12345678` are rejected). `null` uses the stack's default. |
| `matter.discriminator` | integer or `null` | `null` | startup only | Long discriminator, 0 to 4095. `null` uses the stack's default. |
