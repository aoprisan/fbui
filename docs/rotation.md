# Display rotation

Portrait-mounted screens are the norm in signage and kiosks, but panels almost
always scan out landscape. fbui rotates in software, at the copy-out stage —
no DRM plane properties required, so it works identically on DRM, fbdev, and
the terminal backend.

## Using it

Set `FBUI_ROTATE` to the number of degrees the UI should be turned
**clockwise** on the unrotated panel:

```sh
FBUI_ROTATE=90 cargo run -p fbui --example showcase --features platform
```

Pick the value that makes the UI upright for how the panel is mounted:

| Panel mounting | `FBUI_ROTATE` |
|---|---|
| normal landscape | unset / `0` |
| stood on its **left** edge (portrait) | `90` |
| upside down | `180` |
| stood on its **right** edge (portrait) | `270` |

An invalid value is a hard startup error, like the other `FBUI_*` toggles.

## What it does

- The widget tree, layout, and shadow surface all live in **UI orientation**:
  with a 1920×1080 panel and `FBUI_ROTATE=90` your app sees a 1080×1920
  logical surface. Widgets never know the panel is sideways.
- The rotation happens once per frame in the damage-bounded copy-out
  (`Surface::present_to_buffer`): destination rows are still written forward
  and sequentially (the write-combined-memory rule), while reads gather from
  the normal-RAM shadow. Only damaged regions are transformed, so an idle or
  mostly-idle UI pays nothing extra.
- Input (touch, mouse) is mapped from panel coordinates back into UI
  coordinates, so taps land on what they visually hit. Relative pointer motion
  moves the panel-space cursor; the mapping happens when events are delivered.
- The remote console (`FBUI_REMOTE`) sees the UI-orientation frame, and its
  injected clicks are mapped back to panel space, so remote operation is
  unaffected by rotation.

## Custom embedders

The pieces are public in `fbui-render`:

- `Rotation::surface_size` — the UI-orientation dimensions for a panel.
- `Surface::set_rotation` — rotate the copy-out; `present_to_buffer` then
  returns destination-space (panel) rects ready for `Display::present`.
- `Rotation::map_panel_point` / `map_rect` / `map_delta` — the input-side
  mappings the `fbui` runner uses; apply the same in a custom runner.
