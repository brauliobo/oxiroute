function inputValue(event: Event): string {
  return (event.target as HTMLInputElement | HTMLSelectElement).value
}

export function nullableInput(event: Event): string | null {
  return inputValue(event) || null
}

export function nullableIntegerInput(event: Event): number | null {
  const value = inputValue(event)
  return value === '' ? null : Number(value)
}
