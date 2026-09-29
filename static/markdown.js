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
  // Kept in step with EARLY_PREFERENCES in crates/symbol/src/markdown.rs.
  const CHOICES = {
    theme: ["auto", "light", "dark", "sepia"],
    font: ["sans", "serif", "mono"],
    width: ["narrow", "wide", "full"],
  };
  const SCALES = [0.85, 0.92, 1, 1.08, 1.17, 1.28];
  const DEFAULT_SCALE = SCALES.indexOf(1);

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

  function wireControls() {
    const settings = document.querySelector(".symbol-settings");
    if (settings === null) {
      return;
    }
    settings.hidden = false;
    const summary = settings.querySelector("summary");

    const choices = settings.querySelectorAll("[data-symbol-set]");
    const smaller = settings.querySelector('[data-symbol-action="smaller"]');
    const larger = settings.querySelector('[data-symbol-action="larger"]');
    const reset = settings.querySelector('[data-symbol-action="reset-size"]');

    let scale = Number.parseInt(store.get("scale") ?? "", 10);
    if (!Number.isInteger(scale) || scale < 0 || scale >= SCALES.length) {
      scale = DEFAULT_SCALE;
    }

    const refresh = () => {
      for (const button of choices) {
        const pressed = root.dataset[button.dataset.symbolSet] === button.value;
        button.setAttribute("aria-pressed", String(pressed));
      }
      if (smaller) {
        smaller.disabled = scale === 0;
      }
      if (larger) {
        larger.disabled = scale === SCALES.length - 1;
      }
      if (reset) {
        reset.textContent = `${Math.round(SCALES[scale] * 100)}%`;
      }
    };

    const setScale = (index) => {
      scale = Math.min(SCALES.length - 1, Math.max(0, index));
      root.style.setProperty("--md-scale", String(SCALES[scale]));
      store.set("scale", String(scale));
    };

    settings.addEventListener("click", (event) => {
      const choice = event.target.closest("[data-symbol-set]");
      if (choice !== null) {
        const key = choice.dataset.symbolSet;
        if (CHOICES[key]?.includes(choice.value)) {
          root.dataset[key] = choice.value;
          store.set(key, choice.value);
        }
        refresh();
        return;
      }
      switch (event.target.closest("[data-symbol-action]")?.dataset.symbolAction) {
        case "smaller":
          setScale(scale - 1);
          break;
        case "larger":
          setScale(scale + 1);
          break;
        case "reset-size":
          setScale(DEFAULT_SCALE);
          break;
        default:
          return;
      }
      refresh();
    });

    // The panel is a <details>; close it like a menu.
    document.addEventListener("click", (event) => {
      if (settings.open && !settings.contains(event.target)) {
        settings.open = false;
      }
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && settings.open) {
        settings.open = false;
        summary?.focus();
      }
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
