// Render ```mermaid fences with a theme that follows the *site* theme toggle
// (mdBook's <html class="…">), not the OS colour scheme. The library is only
// fetched on pages that contain a diagram, from a pinned CDN build, so the
// repository no longer carries a multi-megabyte bundle.
(function () {
    var MERMAID_URL = "https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.esm.min.mjs";
    var DARK_THEMES = ["ayu", "coal", "navy"];

    var blocks = document.querySelectorAll("code.language-mermaid");
    if (blocks.length === 0) return;

    var sources = [];
    blocks.forEach(function (block, i) {
        var pre = block.parentElement;
        var div = document.createElement("div");
        div.className = "mermaid";
        div.setAttribute("data-mermaid-index", String(i));
        div.textContent = block.textContent;
        sources.push(block.textContent);
        pre.replaceWith(div);
    });

    function isDark() {
        var classes = document.documentElement.classList;
        return DARK_THEMES.some(function (t) { return classes.contains(t); });
    }

    function palette(dark) {
        var css = getComputedStyle(document.documentElement);
        var v = function (name, fallback) { return (css.getPropertyValue(name) || fallback).trim(); };
        return {
            background: v("--bg", dark ? "#12151b" : "#ffffff"),
            primaryColor: dark ? "#242749" : "#e9eafb",
            primaryTextColor: v("--fg", dark ? "#e6e9ef" : "#161a22"),
            primaryBorderColor: dark ? "#8f92f2" : "#5357d2",
            lineColor: dark ? "#9aa3b2" : "#5b6472",
            secondaryColor: dark ? "#1a1e26" : "#f4f5f8",
            tertiaryColor: dark ? "#20242e" : "#eceef4",
            fontFamily: "IBM Plex Sans, system-ui, sans-serif",
            fontSize: "14px",
        };
    }

    var mermaid = null;

    function render() {
        if (!mermaid) return;
        var dark = isDark();
        mermaid.initialize({
            startOnLoad: false,
            theme: "base",
            themeVariables: palette(dark),
            darkMode: dark,
            securityLevel: "loose",
            flowchart: { useMaxWidth: true, htmlLabels: true },
        });
        document.querySelectorAll(".mermaid").forEach(function (div) {
            var i = Number(div.getAttribute("data-mermaid-index"));
            div.removeAttribute("data-processed");
            div.textContent = sources[i];
        });
        mermaid.run({ querySelector: ".mermaid" });
    }

    import(MERMAID_URL).then(function (mod) {
        mermaid = mod.default;
        render();
        // Re-render when the reader switches theme (mdBook swaps the html class).
        var last = isDark();
        new MutationObserver(function () {
            var now = isDark();
            if (now !== last) { last = now; render(); }
        }).observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    }).catch(function (err) {
        console.warn("mermaid failed to load; leaving diagram source visible", err);
        document.querySelectorAll(".mermaid").forEach(function (div) {
            var pre = document.createElement("pre");
            pre.textContent = div.textContent;
            div.replaceWith(pre);
        });
    });
})();
