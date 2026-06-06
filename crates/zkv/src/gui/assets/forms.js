(function() {
  function harden(el) {
    if (!el || el.__zkvHardened) return;
    var t = (el.getAttribute("type") || "").toLowerCase();
    if (t === "checkbox" || t === "radio" || t === "range" || t === "color") return;
    el.__zkvHardened = true;
    el.setAttribute("spellcheck", "false");
    el.setAttribute("autocorrect", "off");
    el.setAttribute("autocapitalize", "off");
    el.setAttribute("autocomplete", "off");
    try {
      el.spellcheck = false;
    } catch (_) {
    }
  }
  function scan(root) {
    if (!root || root.nodeType !== 1) return;
    if (root.matches && root.matches("input, textarea")) harden(root);
    if (root.querySelectorAll) root.querySelectorAll("input, textarea").forEach(harden);
  }
  document.documentElement.setAttribute("spellcheck", "false");
  function start() {
    scan(document.body);
    new MutationObserver(function(muts) {
      for (var i = 0; i < muts.length; i++) {
        var added = muts[i].addedNodes;
        for (var j = 0; j < added.length; j++) scan(added[j]);
      }
    }).observe(document.body, { childList: true, subtree: true });
  }
  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
