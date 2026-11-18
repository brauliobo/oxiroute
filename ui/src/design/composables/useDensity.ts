import { watchEffect } from 'vue'
import type { Ref } from 'vue'

export const DENSITIES = ['compact', 'comfortable'] as const
export type Density = (typeof DENSITIES)[number]

export function applyDensity(density: Density, root: HTMLElement = document.documentElement): void {
  root.dataset.density = density
}

export function useDensity(density: Ref<Density>): void {
  watchEffect(() => applyDensity(density.value))
}
