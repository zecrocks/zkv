const _icoPascal = (kebab) => kebab.split("-").map((s) => s[0].toUpperCase() + s.slice(1)).join("");
const Icon = ({ name, size = 16, color = "currentColor", strokeWidth = 1.5, className, style, ...rest }) => {
  const reg = window.lucide;
  const kids = reg && reg.icons && reg.icons[name] || reg && reg[_icoPascal(name)] || null;
  if (!kids) return /* @__PURE__ */ React.createElement("svg", { width: size, height: size, "aria-hidden": "true", ...rest });
  return /* @__PURE__ */ React.createElement(
    "svg",
    {
      xmlns: "http://www.w3.org/2000/svg",
      width: size,
      height: size,
      viewBox: "0 0 24 24",
      fill: "none",
      stroke: color,
      strokeWidth,
      strokeLinecap: "round",
      strokeLinejoin: "round",
      className,
      style,
      "aria-hidden": "true",
      ...rest
    },
    kids.map((k, i) => React.createElement(k[0], { key: i, ...k[1] || {} }))
  );
};
window.Icon = Icon;
const PauseGlyph = ({ size = 12, className, style, ...rest }) => /* @__PURE__ */ React.createElement(
  "svg",
  {
    xmlns: "http://www.w3.org/2000/svg",
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "currentColor",
    className,
    style,
    "aria-hidden": "true",
    ...rest
  },
  /* @__PURE__ */ React.createElement("rect", { x: "6", y: "5", width: "4", height: "14", rx: "1.2" }),
  /* @__PURE__ */ React.createElement("rect", { x: "14", y: "5", width: "4", height: "14", rx: "1.2" })
);
window.PauseGlyph = PauseGlyph;
