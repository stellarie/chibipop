# CSS Theming

Style the popup with a CSS file. Share the file to share a theme.

## Quick start

1. Open **Settings**.
2. Click **Customize CSS...** in the Popup group.
3. Edit the CSS. Click **Save & Apply**.
4. The popup repaints immediately.

The file is `popup.css`, beside `chibipop.exe`.
Delete it to revert to the built-in theme.

## Selectors

Each selector targets one popup element.

| Selector | Element |
|---|---|
| `.popup` | The panel itself: background, border, shape, font. |
| `.headword` | The main word (largest text). |
| `.reading` | The kana reading below the headword. |
| `.body` | Definition / gloss text. |
| `.dict-label` | The dictionary name above each gloss block. |
| `.frequency` | The frequency badge in the top-right corner. |
| `.collapsed` | One-line summaries of other results. |
| `.dimmed` | POS tags and other metadata. |
| `.separator` | The horizontal rule between the top card and the rest. |

## Properties

### All text selectors

These apply to `.headword`, `.reading`, `.body`, `.dict-label`,
`.frequency`, `.collapsed`, and `.dimmed`.

| Property | Values | Example |
|---|---|---|
| `color` | `#rrggbb` or `#rgb` | `color: #f0f0f5;` |
| `font-size` | Number with `px` | `font-size: 20px;` |
| `font-weight` | `normal`, `bold`, or `100`-`900` | `font-weight: bold;` |
| `font-style` | `normal` or `italic` | `font-style: italic;` |

### `.popup` only

| Property | Values | Example |
|---|---|---|
| `background-color` | `#rrggbb` or `#rgb` | `background-color: #1a1a2e;` |
| `border-color` | `#rrggbb` or `#rgb` | `border-color: #333344;` |
| `border-radius` | Number with `px` | `border-radius: 8px;` |
| `border-width` | Number with `px` | `border-width: 2px;` |
| `padding` | Number with `px` | `padding: 16px;` |
| `font-family` | Quoted name or bare word | `font-family: "Noto Sans JP";` |
| `opacity` | `0.0` to `1.0` | `opacity: 0.85;` |

### `.separator` only

| Property | Values | Example |
|---|---|---|
| `color` | `#rrggbb` or `#rgb` | `color: #404050;` |
| `height` | Number with `px` | `height: 2px;` |

## Not styleable

These parts of the popup do not respond to CSS.

- **Scan overlay** outlines (debug-facing, not user-facing).
- **Anki button** below the popup.
- **Scrollbar** thumb (always uses the `.dimmed` color).
- **Window size**. Controlled by `max_width_percent` and `max_height_percent` in the TOML.
- **Window position**. Computed from the cursor and monitor edges.
- **Side panel** layout ("See also" column width and gap).
- **Line spacing** within and between blocks (hardcoded at 4px and 10px).

## Value syntax

**Hex colours.** `#rrggbb` (six digits) or `#rgb` (three digits, expanded).
Named colours like `red` are not supported.

**Pixel values.** A number followed by `px`. No spaces between the number and `px`.
Other units (`em`, `rem`, `%`) are not supported.

**Font weight.** `normal` (= 400), `bold` (= 700), or a number from 100 to 900
in steps of 100.

**Font style.** `normal` or `italic`.

**Opacity.** A decimal from `0.0` (invisible) to `1.0` (opaque).

**Font family.** A face name in double or single quotes, or a bare word.
Only one name is accepted (no fallback stacks).

## Comments

Block comments are supported: `/* like this */`.
Line comments (`//`) are not.

## Errors

Parse errors appear in the status bar of the CSS editor.
The format is `Line N: message`.
A bad value does not prevent other properties from applying.

## Sharing a theme

Copy `popup.css` to another chibipop install.
The file stands alone. No other files are needed.

The base theme (`dark` or `light`) is selected in the TOML.
The CSS overrides whichever base is active.
A shared CSS file works on both, but colours designed for dark may not
suit light.

## Example

```css
/* Midnight purple theme */

.popup {
  background-color: #1a1a2e;
  border-color: #2d2d44;
  border-radius: 10px;
  padding: 14px;
  font-family: "Noto Sans JP";
  opacity: 0.92;
}

.headword {
  color: #e0d0ff;
  font-size: 22px;
  font-weight: bold;
}

.reading {
  color: #b0a0c8;
  font-size: 14px;
  font-style: italic;
}

.body {
  color: #d0cce0;
  font-size: 15px;
}

.dict-label {
  color: #8888cc;
  font-size: 12px;
}

.frequency {
  color: #666688;
  font-size: 12px;
}

.collapsed {
  color: #9090a8;
  font-size: 13px;
}

.dimmed {
  color: #606078;
  font-size: 12px;
}

.separator {
  color: #2d2d44;
  height: 1px;
}
```
