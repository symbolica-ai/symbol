// Reader controls and client-side rendering for Markdown pages.
//
// Progressive enhancement only. Without this script a page still reads fine,
// the RAW link still works, math shows its TeX source and code stays plain.
//
// Reader choices persist per origin in localStorage, so they follow a reader
// across every Markdown page on this host. A page's front matter supplies the
// defaults; a reader's own choice, once made, wins.
(() => {
  "use strict";

  const root = document.documentElement;
  const THEMES = ["auto", "light", "dark", "sepia"];
  const WIDTHS = ["narrow", "wide", "full"];
  const SCALES = [0.85, 0.92, 1, 1.08, 1.17, 1.28];
  const THEME_LABELS = { auto: "Auto", light: "Light", dark: "Dark", sepia: "Sepia" };
  const WIDTH_LABELS = { narrow: "Narrow", wide: "Wide", full: "Full" };

  const store = {
    get(key) {
      try {
        return window.localStorage.getItem(`symbol-md-${key}`);
      } catch {
        return null;
      }
    },
    set(key, value) {
      try {
        window.localStorage.setItem(`symbol-md-${key}`, value);
      } catch {
        // Private browsing or disabled storage: the choice lasts this page.
      }
    },
  };

  const next = (values, current) => values[(values.indexOf(current) + 1) % values.length];

  function applyScale(index) {
    root.style.setProperty("--md-scale", String(SCALES[index]));
    root.dataset.scale = String(index);
  }

  function wireControls() {
    const controls = document.querySelector(".symbol-controls");
    if (controls === null) {
      return;
    }
    for (const hidden of controls.querySelectorAll("[hidden]")) {
      hidden.hidden = false;
    }

    const themeButton = controls.querySelector('[data-symbol-action="theme"]');
    const widthButton = controls.querySelector('[data-symbol-action="width"]');
    const label = (button, text) => {
      const span = button?.querySelector("span");
      if (span) {
        span.textContent = text;
      }
    };
    const refresh = () => {
      label(themeButton, THEME_LABELS[root.dataset.theme] ?? "Auto");
      label(widthButton, WIDTH_LABELS[root.dataset.width] ?? "Narrow");
      themeButton?.setAttribute("aria-label", `Theme: ${root.dataset.theme}`);
      widthButton?.setAttribute("aria-label", `Width: ${root.dataset.width}`);
    };

    let scale = Number.parseInt(store.get("scale") ?? "", 10);
    if (!Number.isInteger(scale) || scale < 0 || scale >= SCALES.length) {
      scale = SCALES.indexOf(1);
    }
    applyScale(scale);

    controls.addEventListener("click", (event) => {
      const button = event.target.closest("[data-symbol-action]");
      if (button === null) {
        return;
      }
      switch (button.dataset.symbolAction) {
        case "theme":
          root.dataset.theme = next(THEMES, root.dataset.theme);
          store.set("theme", root.dataset.theme);
          break;
        case "width":
          root.dataset.width = next(WIDTHS, root.dataset.width);
          store.set("width", root.dataset.width);
          break;
        case "smaller":
          scale = Math.max(0, scale - 1);
          applyScale(scale);
          store.set("scale", String(scale));
          break;
        case "larger":
          scale = Math.min(SCALES.length - 1, scale + 1);
          applyScale(scale);
          store.set("scale", String(scale));
          break;
        default:
          return;
      }
      refresh();
    });
    refresh();
  }

  // pulldown-cmark marks math with these classes and leaves the TeX source as
  // escaped text, so rendering is exact: no delimiter scanning, and a `$` in
  // prose or code is never mistaken for math.
  function renderMath() {
    if (typeof window.katex === "undefined") {
      return;
    }
    for (const node of document.querySelectorAll(".markdown-body .math")) {
      if (node.classList.contains("katex-rendered")) {
        continue;
      }
      try {
        window.katex.render(node.textContent ?? "", node, {
          displayMode: node.classList.contains("math-display"),
          throwOnError: false,
          output: "htmlAndMathml",
        });
        node.classList.add("katex-rendered");
      } catch (error) {
        node.title = String(error);
      }
    }
  }

  function highlightCode() {
    if (typeof window.hljs === "undefined") {
      return;
    }
    for (const block of document.querySelectorAll(".markdown-body pre > code")) {
      // Unlabelled blocks stay plain. Guessing a language turns prose and
      // command output into confident nonsense.
      if (!/(?:^|\s)language-(?!math\b)\S+/.test(block.className)) {
        continue;
      }
      window.hljs.highlightElement(block);
    }
  }

  function ready() {
    wireControls();
    renderMath();
    highlightCode();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", ready, { once: true });
  } else {
    ready();
  }
})();
