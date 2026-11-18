export const BUTTON_VARIANTS = ['default', 'primary', 'danger', 'link'] as const
export type ButtonVariant = (typeof BUTTON_VARIANTS)[number]
