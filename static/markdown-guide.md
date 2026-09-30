---
title: Markdown pages on symbol
description: How symbol renders Markdown in the browser, and every option for styling it.
toc: true
data:
  about: The guide is itself a Markdown page. Press RAW to see how it is written.
---
# Markdown pages on symbol

Any `.md` or `.markdown` file you publish opens as a page like this one in a
browser. Tools get the file itself: `curl`, `fetch()`, and the symbol clients
receive exactly the bytes you uploaded.

> [!TIP]
> This guide is an ordinary Markdown page. Press **RAW** at the top right to
> read its source, and the **Aa** button beside it to try the reading
> settings.

## Publishing

Upload a file with any client, then open its URL in a browser.

```sh
printf '%s\n' '# Notes' '' 'Euler: $e^{i\pi} + 1 = 0$' > notes.md
curl -T notes.md ${host}/notes/notes.md    # or: symbol put notes/notes.md notes.md
curl ${host}/notes/notes.md                # the Markdown, byte for byte
open ${host}/notes/notes.md                # the rendered page
```

Who gets what:

| Request | Receives |
| :--- | :--- |
| A browser opening the page | The rendered HTML page |
| `curl`, `fetch()`, the clients, anything else | The file, unchanged |
| `/notes/notes.md/RAW`, from anything | The file, unchanged, always |

A page renders only when a request explicitly prefers HTML, which a browser's
navigation does; the User-Agent is never consulted. `RAW` is the way to link to
a file's source from a page, and it is what the **RAW** button opens. The
clients expose it too: `raw()` in JavaScript, TypeScript, and Python, and
`symbol raw notes/notes.md` in the shell.

Files over 4 MiB, and files that are not UTF-8, are always served as they are.
Rendered pages are cached against the file's content hash, so publishing a
change shows up on the next load and an unchanged page costs nothing to serve
again.

## Writing

Rendering follows [CommonMark](https://commonmark.org) with the GitHub
additions most people expect, and a few more.

| You write | You get |
| :--- | :--- |
| `**bold**`, `*italic*`, `~~struck~~` | **bold**, *italic*, ~~struck~~ |
| `H~2~O`, `2^10^`, `^up^` and `~down~` | H~2~O, 2^10^, ^up^ and ~down~ |
| `` `inline code` `` | `inline code` |
| `<kbd>Ctrl</kbd>`, `<mark>marked</mark>` | <kbd>Ctrl</kbd>, <mark>marked</mark> |
| `[a link](#writing)` | [a link](#writing) |
| `$e^{i\pi} + 1 = 0$` | $e^{i\pi} + 1 = 0$ |
| `A claim.[^source]` | A claim.[^source] |

Superscript and subscript work inside words, as in `H~2~O`. A run that
contains a space stays literal, so `~/notes` and `cost ~5` read as written;
escape a delimiter as `\^` or `\~` to keep it.

### Headings

Every heading gets an id and a `#` anchor that appears on hover, so any section
can be linked to. Ids follow GitHub's style: `## Getting started` becomes
`#getting-started`. Set your own, and classes, in braces:

```markdown
## Getting started {#start .highlight}
```

### Lists and tasks

1. Ordered lists
2. Unordered lists
   - nest by indenting
   - as deep as you like

- [x] Task lists, written `- [x] done`
- [ ] and `- [ ] to do`

Term
: Definition lists: the term on one line, `: definition` on the next.

### Quotes and alerts

> A plain quote, written with `>`.

GitHub-style alerts are quotes whose first line names a kind:

```markdown
> [!NOTE]
> Useful information.
```

> [!NOTE]
> `[!NOTE]` for useful information.

> [!IMPORTANT]
> `[!IMPORTANT]` for what readers must know.

> [!WARNING]
> `[!WARNING]` for something that needs attention, and `[!CAUTION]` for the
> risk of harm. `[!TIP]` is the one at the top of this page.

### Code

Fenced code with a language name is highlighted; code without one stays plain.

```python
def greet(name: str) -> str:
    return f"hello, {name}"
```

```
Unlabelled blocks are left exactly as written.
```

Highlighting covers the common languages: Python, JavaScript, TypeScript, Rust,
Go, C and C++, Java, shell, SQL, JSON, YAML, HTML, CSS, Markdown, diff, and
more.

### Math

Math is typeset by KaTeX: `$...$` inline, `$$...$$` for display, or a fenced
block marked `math`.

$$
\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$

````markdown
```math
\mathbf{F} = m\mathbf{a}
```
````

```math
\mathbf{F} = m\mathbf{a}
```

A `$` inside code is never mistaken for math. Copying typeset math copies its
TeX source.

### Tables

| Left | Centre | Right |
| :--- | :---: | ---: |
| `:---` | `:---:` | `---:` |
| aligns left | aligns centre | aligns right |

### Footnotes

Write `[^label]` where the note belongs and `[^label]: text` anywhere in the
file. Notes are numbered in order of use and collected at the end of the page,
each linked back to where it was cited.

### Images and raw HTML

`![Alt text](picture.png)` shows an image; relative paths resolve against the
page, as in any HTML file.

HTML passes through untouched, `<script>` and `<style>` included, so anything a
web page can do, a Markdown page can do:

<details>
<summary>An HTML <code>&lt;details&gt;</code> element</summary>

Markdown works inside HTML blocks when a blank line separates them.

</details>

<p>
  <a class="md-button" href="#styling">A link styled as a button</a>
  <button type="button" onclick="this.textContent = 'Pressed'">A button</button>
</p>

## Front matter

Optional settings go at the very top of the file, as YAML between `---` lines
or TOML between `+++` lines:

```yaml
---
title: Field notes
theme: sepia
font: serif
toc: true
css: ":root { --md-accent: #b5451b }"
---
```

```toml
+++
title = "Field notes"
theme = "sepia"
toc = true
+++
```

Front matter must be closed. A lone `---` at the top of a file with no closing
line is a horizontal rule, as Markdown defines it.

| Option | Value | Default |
| :--- | :--- | :--- |
| `title` | the page title | the first `#` heading, then the file name |
| `description` | `<meta name="description">` | none |
| `lang` | `<html lang>` | `en` |
| `theme` | `auto`, `light`, `dark`, `sepia` | `auto`, which follows the reader's system |
| `font` | `sans`, `serif`, `mono` | `sans` |
| `width` | `narrow`, `medium`, `wide`, `full` | `medium` |
| `toc` | `true` for a table of contents from `##` headings down | `false` |
| `controls` | `true`, `false`, or a list of `theme`, `font`, `width`, `size`, `raw` | `true` |
| `css` | CSS added after the page's own | none |
| `stylesheet`, `stylesheets` | a URL, or a list | none |
| `script`, `scripts` | a URL, or a list, run after the page's own | none |
| `head` | raw HTML for `<head>` | none |
| `class` | classes for `<body>` | none |
| `math` | `false` to turn off KaTeX | `true` |
| `highlight` | `false` to turn off code highlighting | `true` |
| `smart_punctuation` | `true` for curly quotes and dashes | `false` |
| `data` | anything at all: your own values | none |

Only these options are allowed at the top level, so a misspelt one is caught
rather than silently ignored. Put values of your own, such as an author or
tags, under `data`. Scripts on the page can read all of the front matter as
JSON:

```js
const matter = JSON.parse(document.getElementById("symbol-front-matter").textContent);
console.log(matter.data);
```

## Styling

### Reading controls

Every page has a **RAW** button and an **Aa** button that opens settings for
theme, font, width, and text size. A reader's choices are remembered per host,
so they follow the reader to every Markdown page on this server, and they win
over the page's front matter. Hide some or all of the controls with `controls`.

### Your own CSS

Every colour, font, and measure is a CSS custom property, so a line of `css` in
the front matter restyles a page without replacing its stylesheet:

```yaml
css: |
  :root { --md-primary: #ff8a65; --md-primary-edge: #c75b39; --md-primary-fg: #fff }
  :root[data-theme="dark"] { --md-bg: #101418 }
```

| Property | Controls |
| :--- | :--- |
| `--md-bg`, `--md-fg`, `--md-muted` | page background, text, and secondary text |
| `--md-accent` | links, footnote numbers, focus rings |
| `--md-primary`, `--md-primary-edge`, `--md-primary-fg` | buttons: face, bottom edge, label |
| `--md-surface`, `--md-raised`, `--md-edge`, `--md-border` | inline code, cards, their bottom edge, rules |
| `--md-mark` | highlighted text |
| `--md-font`, `--md-font-heading`, `--md-font-ui`, `--md-mono` | body, headings, controls, code |
| `--md-font-sans`, `--md-font-serif` | the stacks behind the `font` choices |
| `--md-measure`, `--md-line-height`, `--md-radius` | text width, line spacing, corner rounding |
| `--md-toc-marker` | the `*` bullets in the table of contents |
| `--md-note`, `--md-tip`, `--md-important`, `--md-warning`, `--md-caution` | alert colours |
| `--md-code-keyword`, `--md-code-string`, `--md-code-number`, `--md-code-comment`, `--md-code-title`, `--md-code-type`, `--md-code-attr` | highlighting |

Themes, fonts, and widths are attributes on `<html>`, so CSS can target them:
`:root[data-theme="dark"]`, `[data-font="serif"]`, `[data-width="wide"]`.

The page is built from a few stable classes: `.symbol-markdown` for the main
column, `.markdown-body` for your content, `.symbol-toc`, `.symbol-footnotes`,
`.symbol-controls`, and `.symbol-problems`.

### Buttons

The raised button style is yours to use. Plain `<button>` elements get it
automatically; add `class="md-button"` to a link, and `md-button-plain` for the
neutral version used in the settings panel.

## Scripts

Scripts can come from `script` in the front matter, from `head`, or from
`<script>` elements anywhere in the page. Scripts named in the front matter run
after the page's own, so math is already typeset and code highlighted when they
start.

## When something is wrong

A page with mistakes still renders, using defaults where a value could not be
used, and lists every problem in a notice at the top, with line numbers where
they help. The notice appears for:

- front matter that does not parse, is not a set of `key: value` pairs, or is
  never closed;
- unknown options, with a suggestion when one looks like a typo, and options
  given twice;
- values of the wrong kind, or outside the allowed choices;
- footnotes used but never written, written but never used, or written twice;
- headings that share an id;

and, once the page is open in a browser:

- stylesheets, scripts, images, or media that fail to load;
- scripts that throw an error;
- math that KaTeX cannot typeset;
- links to `#somewhere` on the page that nothing is called.

Everyone who opens the page sees the notice, so it is worth fixing what it
lists.

## For developers

The [HTTP protocol reference](${host}/API/CURL) covers the details: the
`Accept` rules, `Vary`, `ETag` revalidation, and the `Link` header that points
each rendered page at its `RAW` source.

[^source]: Footnotes like this one are collected at the end of the page, each
    with a link back to where it was cited.
