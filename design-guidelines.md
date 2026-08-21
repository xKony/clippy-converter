# Design Guidelines

Reference for UI work on Clippy Converter. Values here are implemented in
`src/theme.rs` — change them there, then update this file to match.

## Philosophy

- **Quiet chrome, loud content.** The popup is glanced at for two seconds.
  Numbers and units carry all the contrast; every container, border, wash,
  and icon stays close to the background so content never competes with the
  frame around it.
- **One highlight at a time.** There is a single "current row" state. Mouse
  hover does not paint a second highlight color — it *steers* the keyboard
  cursor (`list_cursor`), so what you see highlighted is always exactly one
  row, whichever input moved it there.
- **Gentle by default.** If a new state needs attention, reach for the
  lowest alpha that is still perceptible on the dark fills (~4–8% white),
  not a saturated color. Reserve the accent blue for text selection and
  focus only.

## Color palette

| Token | Value | Use |
|---|---|---|
| `panel_fill` | `#121212` | Popup / root background |
| `window_fill` | `#181818` | Cards, settings surface |
| `extreme_bg_color` | `#0C0C0C` | Text inputs, wells |
| Border stroke | `#2D2D2D` @ 1px | Noninteractive separators |
| Secondary text | `#B4B4B4` @ 1px stroke | Labels, weak text |
| Row cursor wash | `white` @ 20/255 alpha | The single highlighted list row |
| Accent (selection) | `#508CFA` | Text selection / focus ring only |
| Favorite tint | `#FFD700` | Star icon when favorited |
| Success / error toast | `GREEN` / `RED` | "Copied!" / failure hints |

Rules:

- New grays must be neutral (R=G=B) or near-neutral. No blue-gray drift.
- Never use the accent as a fill behind body text; it is too loud.
- Washes are `Color32::from_white_alpha(n)` with `n ≤ 20`. If 20 is not
  enough to make something noticeable, it probably should not be noticed.

## Corner radii

| Surface | Radius |
|---|---|
| OS window (popup & settings) | DWM-rounded on Win 11+ (`placement::round_window_corners`) |
| egui window corner radius | 16 logical px |
| Widgets (buttons, inputs) | 8 |
| List rows | 8 (`theme::ROW_CORNER_RADIUS`) |

Windows 10 falls back to square OS corners silently — do not add custom
transparency masks to fake rounding; the DWM path is the whole solution.
The visible OS shadow of a DWM-rounded borderless window is drawn by
Windows itself and is not tunable; do not try to fake a softer one.

## Popup sizing

The converter popup is **mode-adaptive** (`ui.rs`, `CONVERTER_COMPACT_SIZE`
vs `CONVERTER_INNER_SIZE`):

- Bare value input with no recent history: **330×250** — never leave a dead
  zone below the input field.
- Unit list / results / recent history visible: **330×400**.

New modes must declare which size they need in the per-frame size sync;
send `ViewportCommand::InnerSize` only when the desired size changes.

## Row states (unit picker & results)

- **Rest:** transparent fill, no stroke. Rows separate via spacing + hairline
  separators (results list), not boxes.
- **Current row** (keyboard cursor, or last row the mouse was over): fill
  `theme::row_cursor_wash()`, radius 8. Hovering any row sets the cursor to
  that row — Enter then acts on the row under the mouse, which is why hover
  does not get its own visual state.
- **Pressed:** egui default widget active state.
- Rows must never shift layout between states: apply margins/spacing to all
  rows, not just the highlighted one.

## Spacing

- Item spacing: 10×10, window margin 12, button padding 8×4 (`theme.rs`);
  popup content frame margin 12.
- List rows: 2 px vertical breathing room plus separators; keep row inner
  margin symmetric `(6, 4)` so text aligns across states.

## Typography

| Style | Size | Notes |
|---|---|---|
| Heading | 20 | Mode title ("Enter value", …) |
| Body / Button | 16 | Default |
| Result value | 18, strong | The number is the loudest element in the popup |
| Unit caption | 14, weak color | Under or beside values |

## Spacing

- Item spacing: 10×10, window margin 15, button padding 8×4 (`theme.rs`).
- List rows: 2 px vertical breathing room plus separators; keep row inner
  margin symmetric `(6, 4)` so text aligns across states.

## Motion & repaint discipline

- Event-driven repaints only. Any new animated state must schedule its own
  wake-up (`ctx.request_repaint_after`) instead of repainting continuously.
- Transitions between modes (popup ↔ settings) are instant cuts. No fade or
  slide animations — perceived speed is a feature of this app.

## Do / Don't

- Do reuse tokens from `theme.rs`; don't inline hex colors at call sites.
- Do keep interactive targets ≥ ~28 px tall.
- Don't introduce light mode piecemeal — every color above has a hardcoded
  dark assumption; a theme refactor is a project of its own.
- Don't add shadows back; the flat look is intentional (`window_shadow`,
  `popup_shadow` = NONE).
