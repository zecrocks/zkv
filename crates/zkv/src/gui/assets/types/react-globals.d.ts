// Bridge the vendored, classic-runtime React to @types/react.
//
// The source files use `React.useState(...)` and JSX without importing
// React: `React` is a vendored global (window.React). @types/react ships a
// UMD global (`export as namespace React`) plus a global `JSX` namespace, both
// of which are visible to our script-mode source files (module: none), so no
// `import React` is needed. We only add the `window.React` typing here.
import type * as ReactNS from "react";

declare global {
  interface Window {
    React: typeof ReactNS;
  }
}

export {};
