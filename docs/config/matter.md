# Matter

```yaml
matter:
  storage_dir: /var/lib/it-matters
  passcode: null
  discriminator: null
```

| Option | Type | Default | Applies | Description |
|---|---|---|---|---|
| `matter` | map | see below | startup only | The Matter side of the bridge. |
| `matter.storage_dir` | path | `/var/lib/it-matters` | startup only | Where the bridge keeps its state; created if missing, so the daemon needs write access to it (or its parent). Contents: `matter/` (fabrics and commissioning data kept by the Matter stack), `endpoints.json` (the persisted endpoint ids, see below) and `commissioning.json` (generated pairing codes, mode `0600`). Back it up to keep your controllers paired; delete it to start from scratch (every controller must then pair again). |
| `matter.passcode` | integer or `null` | `null` | startup only | Setup passcode, 1 to 99999998; trivial values such as `11111111`, `12345678` and `87654321` are rejected at startup by `check`. `null` generates a random passcode on first start and keeps it in `commissioning.json`. A value set here always wins over the stored one and is never written to disk by the daemon. Setting a fixed, well-known passcode (such as the Matter test code `20202021`) lets anyone on your network commission an unpaired bridge: use it for testing only. |
| `matter.discriminator` | integer or `null` | `null` | startup only | Long discriminator, 0 to 4095, that tells controllers apart several uncommissioned devices. `null` generates a random one on first start (kept in `commissioning.json`). |

**The UDP port is not configurable.** The Matter stack (rs-matter) listens on the standard Matter UDP port
5540. Open it in the host's firewall; two bridges cannot share a host.

## Pairing codes

The manual pairing code and QR code are printed in the log (at `info` level) when the bridge starts
**and is not yet commissioned**; after a controller has paired it they are no longer printed. Run
`RUST_LOG=info it-matters run` and look for `SetupQRCode` and `PairingCode`. See the
[usage guide](../usage.md#commissioning-and-controllers).

## Endpoint ids

Each unit of each device (see [what controllers see](../matter.md)) becomes one Matter endpoint. Its id
is allocated on first registration (starting at 2; 0 is the root and 1 the Aggregator), saved in
`endpoints.json` and **never reused**: the same plugin device keeps the same endpoint across restarts,
reconnects and even unregister/register cycles. If a device's capabilities change shape (for example a
sensor's role changes) it gets a new endpoint, because Matter does not allow an endpoint to change shape.
Do not edit `endpoints.json`; a corrupt file stops the daemon rather than being reset, since a reset would
hand controllers different devices under ids they already know.
