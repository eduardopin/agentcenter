(() => {
  "use strict";

  const copyText = async (text) => {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return;
    }

    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    document.body.append(textarea);
    textarea.select();
    const copied = document.execCommand("copy");
    textarea.remove();
    if (!copied) throw new Error("Copy command was not available");
  };

  document.querySelectorAll("[data-copy-target]").forEach((button) => {
    const label = button.querySelector(".copy-label");
    if (!label) return;

    const originalLabel = label.textContent;
    const liveRegion = document.getElementById("copy-status");
    const description = button.getAttribute("aria-label")?.replace(/^Copy /, "") || "text";
    let copying = false;
    let resetTimer;

    button.addEventListener("click", async () => {
      if (copying) return;

      const target = document.getElementById(button.dataset.copyTarget);
      if (!target) return;

      copying = true;
      if (resetTimer) window.clearTimeout(resetTimer);
      if (liveRegion) liveRegion.textContent = "";

      try {
        await copyText(target.innerText);
        label.textContent = "Copied";
        if (liveRegion) liveRegion.textContent = `Copied ${description}.`;
      } catch {
        label.textContent = "Select text";
        if (liveRegion) liveRegion.textContent = `Copy unavailable. Select ${description} manually.`;
      } finally {
        copying = false;
      }

      resetTimer = window.setTimeout(() => {
        label.textContent = originalLabel;
        resetTimer = undefined;
      }, 1800);
    });
  });
})();
