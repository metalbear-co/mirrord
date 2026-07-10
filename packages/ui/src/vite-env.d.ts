/// <reference types="vite/client" />

// Asset module declarations, needed because the shell's typecheck follows imports into the feature
// packages (`packages/monitor`, `packages/wizard`), which import images directly.
declare module '*.svg' {
  const content: string
  export default content
}

declare module '*.png' {
  const content: string
  export default content
}
