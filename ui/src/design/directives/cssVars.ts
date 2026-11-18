import { watchEffect } from 'vue'

// Function directive that sets CSS custom properties from a template, for values only known at runtime (bar fills,
// layout coordinates). They go through the CSSOM, which a strict `style-src 'self'` allows, unlike `:style` or
// `style=` bindings that Vue applies as a `style` attribute. The stylesheet reads them with var().
// A Vapor directive receives the bound value as a getter and runs once, so it tracks the getter itself.
export function vCssVars(element: HTMLElement, vars: () => Record<string, string | number>): void {
  let applied: string[] = []
  watchEffect(() => {
    const next = vars()
    for (const name of applied) if (!(name in next)) element.style.removeProperty(name)
    for (const [name, value] of Object.entries(next)) element.style.setProperty(name, String(value))
    applied = Object.keys(next)
  })
}
