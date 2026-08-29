(function () {
    if (typeof mermaid === "undefined") return;

    mermaid.initialize({
        startOnLoad: false,
        theme: window.matchMedia("(prefers-color-scheme: dark)").matches
            ? "dark"
            : "default",
        securityLevel: "loose",
        flowchart: { useMaxWidth: true, htmlLabels: true },
    });

    document.addEventListener("DOMContentLoaded", function () {
        var blocks = document.querySelectorAll("code.language-mermaid");
        blocks.forEach(function (block, i) {
            var pre = block.parentElement;
            var div = document.createElement("div");
            div.className = "mermaid";
            div.textContent = block.textContent;
            pre.replaceWith(div);
        });
        mermaid.run({ querySelector: ".mermaid" });
    });
})();
