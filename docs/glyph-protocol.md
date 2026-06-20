# Glyph Protocol

{{since('nightly', inline=True)}}

The **Glyph Protocol** lets terminal applications ship their own vector
glyphs to WezTerm at runtime — monochrome icons via OpenType `glyf`, or
flat layered color via OpenType `COLR` v0 — without requiring the user to
install a patched font (Nerd Fonts, Powerline, and friends).

Registrations are restricted to the three Unicode Private Use Areas, which
the user never types and ordinary text never contains, so the protocol
**cannot** be used to change the appearance of real text. The cell buffer
always stores the registered codepoint, so selection, copy, and search
return that codepoint — never glyph pixels.

It is the dynamic, per-session cousin of [custom_block_glyphs](config/lua/config/custom_block_glyphs.md):
WezTerm already synthesizes box-drawing and similar glyphs itself; the
Glyph Protocol lets applications supply their own at runtime.

This feature is **off by default**; enable it with
[enable_glyph_protocol](config/lua/config/enable_glyph_protocol.md):

```lua
config.enable_glyph_protocol = true
```

Protocol reference: <https://rapha.land/introducing-glyph-protocol-for-terminals/>

## Transport

Commands are sent as APC sequences framed by `ESC _ … ESC \`, with a body
that begins with the protocol identifier `25a1` (U+25A1 WHITE SQUARE)
followed by a single-byte verb and `;`-separated `key=value` parameters:

```
ESC _ 25a1 ; <verb> [ ; key=value ]* [ ; <base64-payload> ] ESC \
```

## Verbs

| Verb | Meaning |
|------|---------|
| `s` | Support negotiation. The terminal replies with the payload formats it accepts: `ESC _ 25a1;s;fmt=glyf,colrv0 ESC \`. No reply means the protocol is unsupported (or disabled). |
| `q;cp=<hex>` | Query coverage of a codepoint. Replies with `status=` (a set drawn from `glossary`, `system`). |
| `r;cp=<hex>;…;<base64>` | Register a glyph at a PUA codepoint. |
| `c[;cp=<hex>]` | Clear one registration, or all of them when `cp` is omitted. |

## Registering a glyph

The `r` verb accepts these parameters (all optional except `cp` and the
payload):

| Parameter | Meaning | Default |
|-----------|---------|---------|
| `cp` | Target codepoint, hex. Must be in a PUA range. | — |
| `fmt` | Payload format: `glyf` or `colrv0`. | `glyf` |
| `reply` | `0` silent, `1` success+failure, `2` failures only. | `1` |
| `upm` | Units per em the outline is authored in. | `1000` |
| `aw` | Authored advance width, in `upm` units. | `upm` |
| `lh` | Authored line height, in `upm` units. | `upm` |
| `width` | Cell width override: `1` or `2`. Authoritative for layout. | `1` |
| `size` | Scale policy: `height`, `advance`, `contain`, `cover`, `stretch`. | `height` |
| `align` | Placement as `<h>,<v>`; `<h>` ∈ {start, center, end}, `<v>` ∈ {start, center, end, baseline}. | `center,center` |
| `pad` | Span insets as fractions `<t>,<r>,<b>,<l>`. | `0,0,0,0` |

The glyph is scaled and positioned at render time from these parameters
and the terminal's current cell metrics, so it scales correctly when the
font size changes with no re-registration.

### Example

Register an upward triangle at `U+E000` and print it (Python):

```python
import base64, sys

# A bare OpenType simple-glyph record (upm=1000, Y-up).
payload = base64.b64encode(open("triangle.glyf", "rb").read()).decode()
sys.stdout.write(f"\x1b_25a1;r;cp=E000;upm=1000;{payload}\x1b\\")
sys.stdout.write("icon: \n")
sys.stdout.flush()
```

## Color glyphs

`fmt=colrv0` carries a layered flat-color glyph: a small container wrapping
the simple-glyph outlines each layer references plus the OpenType `COLR` v0
and `CPAL` tables. Layers composite in painter order. A layer whose palette
index is `0xFFFF` is painted in the cell's current foreground color and
re-rasterizes when the foreground changes.

## Limitations

The implementation targets a safe, correct, single-host v1. The following
are **not** yet supported:

- **`colrv1`** (the COLR v1 paint graph: gradients, transforms, composites).
  Only `glyf` and `colrv0` are advertised; a `colrv1` registration is
  rejected with `reason=malformed_payload`.
- **Accurate `q` system-font coverage.** Query results currently report
  glossary coverage only; `system` coverage against the GUI font stack is
  not yet wired up.
- **Multiplexer / remote panes.** The glossary lives in the local terminal
  and does not cross the `wezterm connect` / SSH-mux boundary (the same
  limitation that affects images).
