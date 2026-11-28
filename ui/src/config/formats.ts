export const CONFIG_FORMATS = ['oxi', 'kdl', 'yaml', 'toml', 'lua', 'hocon', 'uci'] as const

export type ConfigFormat = (typeof CONFIG_FORMATS)[number]

export interface ConfigFormatSpec {
  readonly wireName: ConfigFormat
  readonly label: string
  readonly extension: string
  readonly supportsNull: boolean
}

export const CONFIG_FORMAT_SPECS: Readonly<Record<ConfigFormat, ConfigFormatSpec>> = {
  oxi: { wireName: 'oxi', label: 'OXI', extension: '.oxi', supportsNull: true },
  kdl: { wireName: 'kdl', label: 'KDL', extension: '.kdl', supportsNull: true },
  yaml: { wireName: 'yaml', label: 'YAML', extension: '.yaml', supportsNull: true },
  toml: { wireName: 'toml', label: 'TOML', extension: '.toml', supportsNull: false },
  lua: { wireName: 'lua', label: 'Lua', extension: '.lua', supportsNull: true },
  hocon: { wireName: 'hocon', label: 'HOCON', extension: '.hocon', supportsNull: true },
  uci: { wireName: 'uci', label: 'UCI', extension: '.uci', supportsNull: true },
}

export function isConfigFormat(value: unknown): value is ConfigFormat {
  return typeof value === 'string' && (CONFIG_FORMATS as readonly string[]).includes(value)
}

export function formatLabel(format: ConfigFormat): string {
  return CONFIG_FORMAT_SPECS[format].label
}

export function formatExtension(format: ConfigFormat): string {
  return CONFIG_FORMAT_SPECS[format].extension
}
