import { beforeAll } from 'vitest'

// jsdom has no modal <dialog>: showModal() opens it and close() closes it and fires `close`, as browsers do.
beforeAll(() => {
  const proto = HTMLDialogElement.prototype
  proto.showModal = function showModal(this: HTMLDialogElement) { this.setAttribute('open', '') }
  proto.close = function close(this: HTMLDialogElement) {
    if (!this.open) return
    this.removeAttribute('open')
    this.dispatchEvent(new Event('close'))
  }
})
