const editorCard = document.querySelector(".editor-card");
const markdownInput = document.querySelector("#markdown-input");
const markdownPreview = document.querySelector("#markdown-preview");
const titleInput = document.querySelector("#title-input");
const slugInput = document.querySelector("#slug-input");

if (editorCard && markdownInput && markdownPreview) {
  const previewEndpoint = editorCard.dataset.previewEndpoint;
  const modeButtons = Array.from(editorCard.querySelectorAll("[data-mode]"));
  const toolButtons = Array.from(editorCard.querySelectorAll("[data-tool]"));
  const initialMode = editorCard.dataset.editorMode || "split";

  const debounce = (fn, delay) => {
    let timer;
    return (...args) => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => fn(...args), delay);
    };
  };

  const setMode = (mode) => {
    editorCard.dataset.currentMode = mode;
    modeButtons.forEach((button) => {
      button.classList.toggle("is-active", button.dataset.mode === mode);
    });
  };

  const renderPreview = debounce(async () => {
    try {
      const response = await fetch(previewEndpoint, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ markdown: markdownInput.value }),
      });
      if (!response.ok) {
        return;
      }
      const data = await response.json();
      markdownPreview.innerHTML = data.html;
    } catch (_) {
      // Ignore transient preview errors and keep the last preview state.
    }
  }, 180);

  const slugify = (value) =>
    value
      .toLowerCase()
      .trim()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || `note-${crypto.randomUUID().replace(/-/g, "").slice(0, 12)}`;

  const setSelection = (start, end) => {
    markdownInput.focus();
    markdownInput.setSelectionRange(start, end);
  };

  const wrapSelection = (prefix, suffix = prefix, fallback = "text") => {
    const start = markdownInput.selectionStart;
    const end = markdownInput.selectionEnd;
    const selected = markdownInput.value.slice(start, end) || fallback;
    markdownInput.setRangeText(
      `${prefix}${selected}${suffix}`,
      start,
      end,
      "end",
    );
    setSelection(start + prefix.length, start + prefix.length + selected.length);
    renderPreview();
  };

  const prefixLines = (prefix, fallback = "") => {
    const start = markdownInput.selectionStart;
    const end = markdownInput.selectionEnd;
    const selected = markdownInput.value.slice(start, end) || fallback;
    const lines = selected.split("\n");
    const prefixed = lines.map((line) => `${prefix}${line}`.trimEnd()).join("\n");
    markdownInput.setRangeText(prefixed, start, end, "end");
    setSelection(start, start + prefixed.length);
    renderPreview();
  };

  const insertBlock = (text) => {
    const start = markdownInput.selectionStart;
    const end = markdownInput.selectionEnd;
    const padBefore = start > 0 && markdownInput.value[start - 1] !== "\n" ? "\n\n" : "";
    const padAfter = end < markdownInput.value.length && markdownInput.value[end] !== "\n" ? "\n\n" : "";
    const snippet = `${padBefore}${text}${padAfter}`;
    markdownInput.setRangeText(snippet, start, end, "end");
    const cursor = start + snippet.length;
    setSelection(cursor, cursor);
    renderPreview();
  };

  const applyTool = (tool) => {
    switch (tool) {
      case "h1":
        prefixLines("# ", "Heading");
        break;
      case "h2":
        prefixLines("## ", "Heading");
        break;
      case "bold":
        wrapSelection("**", "**", "bold");
        break;
      case "italic":
        wrapSelection("*", "*", "italic");
        break;
      case "quote":
        prefixLines("> ", "Quote");
        break;
      case "ul":
        prefixLines("- ", "List item");
        break;
      case "ol":
        prefixLines("1. ", "List item");
        break;
      case "task":
        prefixLines("- [ ] ", "Task");
        break;
      case "code":
        if (markdownInput.selectionStart !== markdownInput.selectionEnd) {
          wrapSelection("`", "`", "code");
        } else {
          insertBlock("```\ncode\n```");
        }
        break;
      case "link":
        wrapSelection("[", "](https://)", "link");
        break;
      case "hr":
        insertBlock("---");
        break;
      default:
        break;
    }
  };

  modeButtons.forEach((button) => {
    button.addEventListener("click", () => setMode(button.dataset.mode));
  });
  toolButtons.forEach((button) => {
    button.addEventListener("click", () => applyTool(button.dataset.tool));
  });

  markdownInput.addEventListener("input", renderPreview);
  titleInput?.addEventListener("input", () => {
    if (slugInput && !slugInput.dataset.touched) {
      slugInput.value = slugify(titleInput.value);
    }
  });
  slugInput?.addEventListener("input", () => {
    slugInput.dataset.touched = "true";
  });

  setMode(window.matchMedia("(max-width: 960px)").matches ? "write" : initialMode);
  renderPreview();
}
