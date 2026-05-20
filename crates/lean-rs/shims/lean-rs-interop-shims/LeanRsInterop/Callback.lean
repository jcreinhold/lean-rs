import LeanRsInterop.Callback.Tick
import LeanRsInterop.Callback.String

/-! Roll-up module for generic callback ABI helpers.

    Payload-specific helpers live under `LeanRsInterop.Callback.Tick` and
    `LeanRsInterop.Callback.String`. The names describe the ABI payload shape,
    not a downstream policy such as host progress or JSON streaming.
 -/
