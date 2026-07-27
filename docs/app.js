const EXAMPLES = Object.freeze({
  rust: {
    label: "Rust",
    highlightLanguage: "rust",
    source: "projects/rust/src/main.rs",
  },
  python: {
    label: "Python",
    highlightLanguage: "python",
    source: "projects/python/src/demo.py",
  },
  typescript: {
    label: "TypeScript",
    highlightLanguage: "typescript",
    source: "projects/typescript/src/demo.ts",
  },
});

const DEFAULT_LANGUAGE = "rust";

const COMPARISONS = Object.freeze({
  arborium: {
    label: "Arborium",
  },
  highlightjs: {
    label: "Highlight.js",
  },
});

const lighterOutput = document.querySelector("#lighter-output");
const comparisonOutput = document.querySelector("#comparison-output");
const comparisonSource = document.querySelector("#comparison-source");
const codeViewport = document.querySelector("#code-viewport");
const lighterPane = document.querySelector("#lighter-pane");
const comparisonPane = document.querySelector("#comparison-pane");
const scrubberLine = document.querySelector(".scrubber-line");
const languageTabs = document.querySelector(".language-tabs");
const status = document.querySelector("#status");

let selectedExample;
let selectedSource;
let arboriumFragment;
let dividerIsDragging = false;
let comparisonAmount = 35;

const outputPath = (language, highlighter) =>
  `generated/${language}/${highlighter}.html`;

const createTab = ([language, example]) => {
  const tab = document.createElement("button");
  tab.type = "button";
  tab.id = `tab-${language}`;
  tab.dataset.language = language;
  tab.role = "tab";
  tab.textContent = example.label;
  tab.setAttribute("aria-controls", "comparison-output lighter-output");
  tab.addEventListener("click", () => selectLanguage(language));
  return tab;
};

const setSelectedTab = (language) => {
  languageTabs.querySelectorAll("[role='tab']").forEach((tab) => {
    const selected = tab.dataset.language === language;
    tab.setAttribute("aria-selected", selected);
    tab.tabIndex = selected ? 0 : -1;
  });
};

const renderHighlightJs = (source, language, output) => {
  if (!globalThis.hljs) {
    throw new Error("Highlight.js did not load");
  }

  output.innerHTML = globalThis.hljs.highlight(source, {
    language,
    ignoreIllegals: true,
  }).value;
};

const renderComparison = () => {
  const comparison = COMPARISONS[comparisonSource.value];

  comparisonOutput.classList.toggle(
    "hljs",
    comparisonSource.value === "highlightjs",
  );

  if (comparisonSource.value === "highlightjs") {
    renderHighlightJs(
      selectedSource,
      selectedExample.highlightLanguage,
      comparisonOutput,
    );
  } else {
    comparisonOutput.innerHTML = arboriumFragment;
  }

  scrubberLine.setAttribute(
    "aria-valuetext",
    `${100 - comparisonAmount}% Lighter, ${comparisonAmount}% ${comparison.label}`,
  );
};

const syncLighterScroll = () => {
  lighterPane.scrollLeft = comparisonPane.scrollLeft;
  lighterPane.scrollTop = comparisonPane.scrollTop;
};

const setComparisonAmount = (amount) => {
  comparisonAmount = Math.round(Math.min(Math.max(amount, 0), 100));
  codeViewport.style.setProperty("--split", `${100 - comparisonAmount}%`);
  scrubberLine.setAttribute("aria-valuenow", comparisonAmount);
  const comparison = COMPARISONS[comparisonSource.value];
  scrubberLine.setAttribute(
    "aria-valuetext",
    `${100 - comparisonAmount}% Lighter, ${comparisonAmount}% ${comparison.label}`,
  );
};

const updateSplitFromPosition = (clientY) => {
  const bounds = codeViewport.getBoundingClientRect();
  const position = (clientY - bounds.top) / bounds.height;
  const lighterAmount = Math.min(Math.max(position, 0), 1) * 100;
  setComparisonAmount(100 - lighterAmount);
};

const selectLanguage = async (requestedLanguage) => {
  const language = Object.hasOwn(EXAMPLES, requestedLanguage)
    ? requestedLanguage
    : DEFAULT_LANGUAGE;
  const example = EXAMPLES[language];
  const requestPaths = [
    example.source,
    outputPath(language, "arborium"),
    outputPath(language, "lighter"),
  ];

  setSelectedTab(language);
  comparisonSource.disabled = true;
  status.textContent = "";

  try {
    const responses = await Promise.all(
      requestPaths.map((path) => fetch(path)),
    );
    const failedResponse = responses.find((response) => !response.ok);

    if (failedResponse) {
      throw new Error(`Could not load ${failedResponse.url}`);
    }

    const [source, arborium, lighter] = await Promise.all(
      responses.map((response) => response.text()),
    );

    selectedExample = example;
    selectedSource = source;
    arboriumFragment = arborium;
    lighterOutput.innerHTML = lighter;
    lighterOutput.dataset.highlighter = "lighter";
    renderComparison();
    comparisonSource.disabled = false;
    comparisonPane.scrollTo(0, 0);
    syncLighterScroll();

    history.replaceState(null, "", `#${language}`);
  } catch (error) {
    status.textContent = error.message;
  }
};

const handleTabKeys = (event) => {
  const tabs = [...languageTabs.querySelectorAll("[role='tab']")];
  const currentIndex = tabs.indexOf(document.activeElement);
  const keyDirection = { ArrowLeft: -1, ArrowRight: 1 }[event.key];

  if (currentIndex < 0 || !keyDirection) {
    return;
  }

  event.preventDefault();
  const nextIndex = (currentIndex + keyDirection + tabs.length) % tabs.length;
  tabs[nextIndex].focus();
  tabs[nextIndex].click();
};

Object.entries(EXAMPLES)
  .map(createTab)
  .forEach((tab) => languageTabs.append(tab));
languageTabs.addEventListener("keydown", handleTabKeys);
comparisonSource.addEventListener("change", renderComparison);
comparisonPane.addEventListener("scroll", syncLighterScroll);
scrubberLine.addEventListener("mousedown", (event) => {
  event.preventDefault();
  globalThis.getSelection()?.removeAllRanges();
  dividerIsDragging = true;
  codeViewport.classList.add("is-scrubbing");
  updateSplitFromPosition(event.clientY);
});
document.addEventListener("mousemove", (event) => {
  if (dividerIsDragging) {
    event.preventDefault();
    updateSplitFromPosition(event.clientY);
  }
});
document.addEventListener("mouseup", () => {
  dividerIsDragging = false;
  codeViewport.classList.remove("is-scrubbing");
});
scrubberLine.addEventListener(
  "touchstart",
  (event) => {
    dividerIsDragging = true;
    codeViewport.classList.add("is-scrubbing");
    updateSplitFromPosition(event.touches[0].clientY);
    event.preventDefault();
  },
  { passive: false },
);
document.addEventListener(
  "touchmove",
  (event) => {
    if (dividerIsDragging) {
      updateSplitFromPosition(event.touches[0].clientY);
      event.preventDefault();
    }
  },
  { passive: false },
);
document.addEventListener("touchend", () => {
  dividerIsDragging = false;
  codeViewport.classList.remove("is-scrubbing");
});
scrubberLine.addEventListener("keydown", (event) => {
  const changes = {
    ArrowDown: -1,
    ArrowLeft: -1,
    ArrowRight: 1,
    ArrowUp: 1,
    PageDown: -10,
    PageUp: 10,
  };
  const change = changes[event.key];

  if (event.key === "Home") {
    setComparisonAmount(0);
  } else if (event.key === "End") {
    setComparisonAmount(100);
  } else if (change) {
    setComparisonAmount(comparisonAmount + change);
  } else {
    return;
  }

  event.preventDefault();
});

selectLanguage(location.hash.slice(1));
