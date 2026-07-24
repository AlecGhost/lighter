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

const OUTPUTS = Object.freeze({
  arborium: document.querySelector("#arborium-output"),
  lighter: document.querySelector("#lighter-output"),
});

const highlightOutput = document.querySelector("#highlightjs-output");
const languageTabs = document.querySelector(".language-tabs");
const sourcePath = document.querySelector("#source-path");
const status = document.querySelector("#status");

const outputPath = (language, highlighter) =>
  `generated/${language}/${highlighter}.html`;

const createTab = ([language, example]) => {
  const tab = document.createElement("button");
  tab.type = "button";
  tab.id = `tab-${language}`;
  tab.dataset.language = language;
  tab.role = "tab";
  tab.textContent = example.label;
  tab.setAttribute("aria-controls", "highlightjs-output arborium-output lighter-output");
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

const renderHighlightJs = (source, language) => {
  if (!globalThis.hljs) {
    throw new Error("Highlight.js did not load");
  }

  highlightOutput.innerHTML = globalThis.hljs.highlight(source, {
    language,
    ignoreIllegals: true,
  }).value;
};

const selectLanguage = async (requestedLanguage) => {
  const language = Object.hasOwn(EXAMPLES, requestedLanguage)
    ? requestedLanguage
    : DEFAULT_LANGUAGE;
  const example = EXAMPLES[language];
  const requestPaths = [
    example.source,
    ...Object.keys(OUTPUTS).map((highlighter) => outputPath(language, highlighter)),
  ];

  setSelectedTab(language);
  status.textContent = `Loading ${example.label}…`;

  try {
    const responses = await Promise.all(requestPaths.map((path) => fetch(path)));
    const failedResponse = responses.find((response) => !response.ok);

    if (failedResponse) {
      throw new Error(`Could not load ${failedResponse.url}`);
    }

    const [source, ...fragments] = await Promise.all(
      responses.map((response) => response.text()),
    );

    renderHighlightJs(source, example.highlightLanguage);
    Object.entries(OUTPUTS).forEach(([highlighter, element], index) => {
      element.innerHTML = fragments[index];
      element.dataset.highlighter = highlighter;
    });

    sourcePath.textContent = example.source;
    status.textContent = `${example.label} example ready`;
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

Object.entries(EXAMPLES).map(createTab).forEach((tab) => languageTabs.append(tab));
languageTabs.addEventListener("keydown", handleTabKeys);

selectLanguage(location.hash.slice(1));
