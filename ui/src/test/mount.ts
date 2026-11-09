import { DOMWrapper, flushPromises } from '@vue/test-utils'
import { afterEach } from 'vitest'
import { createComponent, createVaporApp, defineVaporComponent, nextTick, proxyRefs, reactive } from 'vue'
import type { Component } from 'vue'

// Component test mount for pure Vapor apps. @vue/test-utils only mounts through the VDOM renderer, which the
// management UI does not ship, so components are mounted with `createVaporApp` and wrapped in the VTU `DOMWrapper`
// (find/get/setValue/trigger/text/attributes...). Only what the tests use is supported:
//   props / setProps  reactive root props (createVaporApp itself only takes static ones)
//   emitted()         events declared with defineEmits, recorded like VTU does
//   vm                state a component publishes with defineExpose
//   attachTo, provide, unmount (also automatic after each test)
export { flushPromises }

interface MountOptions {
  props?: Record<string, unknown>
  attachTo?: Element
  provide?: Record<string, unknown>
}
type Events = Record<string, unknown[][]>
type ExposingInstance = { exposed?: Record<string, unknown> } | null

const handlerName = (event: string) => `on${event[0]!.toUpperCase()}${event.slice(1)}`
const mounted = new Set<VaporWrapper>()
afterEach(() => mounted.forEach((wrapper) => wrapper.unmount()))

export class VaporWrapper extends DOMWrapper<HTMLElement> {
  private readonly host: HTMLElement
  private readonly app: { unmount(): void }
  private readonly props: Record<string, unknown>
  private readonly events: Events
  private readonly instance: ExposingInstance

  constructor(host: HTMLElement, app: { unmount(): void }, props: Record<string, unknown>, events: Events,
    instance: ExposingInstance) {
    super(host)
    this.host = host
    this.app = app
    this.props = props
    this.events = events
    this.instance = instance
  }

  get element(): HTMLElement { return (this.host.firstElementChild ?? this.host) as HTMLElement }
  get vm(): Record<string, unknown> { return proxyRefs({ ...this.instance?.exposed }) }
  emitted(): Events
  emitted(name: string): unknown[][] | undefined
  emitted(name?: string) { return name ? this.events[name] : this.events }
  async setProps(next: Record<string, unknown>): Promise<void> { Object.assign(this.props, next); await nextTick() }
  unmount(): void { if (mounted.delete(this)) { this.app.unmount(); this.host.remove() } }
}

export function mount(component: Component, options: MountOptions = {}): VaporWrapper {
  const host = document.createElement('div')
  options.attachTo?.append(host)
  // Deeply reactive like VTU: several tests mutate nested prop objects in place.
  const props = reactive<Record<string, unknown>>({ ...options.props })
  const events: Events = {}
  const declared = (component as { emits?: string[] | Record<string, unknown> }).emits ?? []
  for (const name of Array.isArray(declared) ? declared : Object.keys(declared)) {
    props[handlerName(name)] ??= (...args: unknown[]) => { (events[name] ??= []).push(args) }
  }
  let instance: ExposingInstance = null
  const Root = defineVaporComponent(() => {
    const created = createComponent(component as never, { $: [() => props] })
    instance = created as unknown as ExposingInstance
    return created
  })
  const app = createVaporApp(Root)
  for (const [key, value] of Object.entries(options.provide ?? {})) app.provide(key, value)
  app.mount(host)
  const wrapper = new VaporWrapper(host, app, props, events, instance)
  mounted.add(wrapper)
  return wrapper
}
