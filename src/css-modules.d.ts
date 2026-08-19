/** CSS Modules imports (compiled by the tsdown dsh-css-modules-inline plugin). */
declare module '*.module.css' {
  const classes: Record<string, string>
  export default classes
}

/** Plain stylesheet imports (injected, no class map — e.g. omt-shared.css). */
declare module '*.css' {
  const classes: Record<string, string>
  export default classes
}
