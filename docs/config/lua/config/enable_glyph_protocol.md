---
tags:
  - appearance
  - font
---
## `enable_glyph_protocol = false`

{{since('nightly', inline=True)}}

When set to `true`, WezTerm enables the **Glyph Protocol**: applications
can register custom vector glyphs (monochrome and color) at Private-Use-Area
codepoints at runtime, transported over APC escape sequences, without
installing a patched font.

The default is `false`. While the feature is under development it is opt-in;
when disabled, glyph-protocol APC messages are ignored and the support
(`s`) query produces no reply, so well-behaved clients detect that the
terminal does not implement the protocol and fall back gracefully.

```lua
config.enable_glyph_protocol = true
```

See [Glyph Protocol](../../../glyph-protocol.md) for the full feature
description, supported payload formats, and current limitations.
