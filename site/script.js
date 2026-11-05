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
    button.addEventListener("click", async () => {
      const target = document.getElementById(button.dataset.copyTarget);
      const label = button.querySelector(".copy-label");
      if (!target || !label) return;

      try {
        await copyText(target.innerText);
        label.textContent = "Copied";
      } catch {
        label.textContent = "Select text";
      }

      window.setTimeout(() => {
        label.textContent = "Copy";
      }, 1800);
    });
  });
})();
