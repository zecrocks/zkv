// Icon.tsx: Lucide wrapper
type IconProps = {
  name: string;
  size?: number;
  color?: string;
  strokeWidth?: number;
  className?: string;
  style?: React.CSSProperties;
  [k: string]: any;
};

const _icoPascal = (kebab: string) =>
  kebab.split('-').map(s => s[0].toUpperCase() + s.slice(1)).join('');

const Icon = ({ name, size = 16, color = 'currentColor', strokeWidth = 1.5, className, style, ...rest }: IconProps) => {
  const reg = window.lucide;
  const kids =
    (reg && reg.icons && reg.icons[name]) ||
    (reg && reg[_icoPascal(name)]) || null;
  if (!kids) return <svg width={size} height={size} aria-hidden="true" {...rest}/>;
  return (
    <svg xmlns="http://www.w3.org/2000/svg" width={size} height={size}
         viewBox="0 0 24 24" fill="none" stroke={color}
         strokeWidth={strokeWidth} strokeLinecap="round" strokeLinejoin="round"
         className={className} style={style} aria-hidden="true" {...rest}>
      {kids.map((k: any, i: number) => React.createElement(k[0], { key: i, ...(k[1] || {}) }))}
    </svg>
  );
};
window.Icon = Icon;

// A plain filled pause glyph (the Lucide `pause` is stroked/outlined, which
// reads as too "loud" at small sizes). Used for the sync pause controls.
type PauseGlyphProps = {
  size?: number;
  className?: string;
  style?: React.CSSProperties;
  [k: string]: any;
};

const PauseGlyph = ({ size = 12, className, style, ...rest }: PauseGlyphProps) => (
  <svg xmlns="http://www.w3.org/2000/svg" width={size} height={size} viewBox="0 0 24 24"
       fill="currentColor" className={className} style={style} aria-hidden="true" {...rest}>
    <rect x="6" y="5" width="4" height="14" rx="1.2" />
    <rect x="14" y="5" width="4" height="14" rx="1.2" />
  </svg>
);
window.PauseGlyph = PauseGlyph;

