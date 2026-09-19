# GUI style conventions

Use the shared helpers and tokens in `src/gui/theme.rs` so related controls look
and behave consistently across pages and detached windows.

- Use sentence case for page titles, buttons, dialog titles and helper text.
  Preserve names such as ProBalance, Gaming Mode, Steam and Lutris, and acronyms
  such as CPU, GPU and RAM. Uppercase table/KPI labels are a distinct, compact
  visual level; do not extend that treatment to sentences or ordinary actions.
- Use bundled DejaVu Sans for text, the explicit bold face for headings and
  primary actions, and DejaVu Sans Mono for aligned numbers and commands.
  `RichText::strong()` changes contrast, not font weight.
- Use the shared size tokens: 12 pt small/column labels, 13.5 pt help, 15 pt body,
  16 pt card headings, 20 pt hero status, 22 pt page titles and 26 pt KPI values.
  Reflow a dense control or reduce axis-label density instead of shrinking text.
- Use `help_text` for secondary explanations and semantic theme colors for
  warnings/errors. Explicitly check contrast in light and dark themes.
- Use MiB/GiB for binary memory sizes and KiB/s or MiB/s for binary I/O rates.
  Decimal bandwidth remains GB/s. Separate a value from its unit: `55 °C`,
  `500 ms`, `2 s`. Keep compact percent readouts consistent within a component.
- Use `…` for actions that open another choice or dialog. Instructions must use
  the exact label of the visible control. Show commands in monospace and avoid
  repeating the same setup instructions in adjacent controls.
- Measure variable-width values such as PIDs. Let selection grids wrap and
  inspect narrow layouts after increasing fonts or button weights.

Run the read-only UI tour for main-page screenshots. Detached windows need their
own inspection. `overlay-settings-preview --light --embedded` renders the real
overlay controls in a capturable light-theme fallback window; it is not a native
window-decoration or Wayland-compositing test.
