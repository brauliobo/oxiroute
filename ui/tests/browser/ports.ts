const BASE_PORT = Number(process.env.OXIROUTE_UI_PORT ?? 4173)

export const ports = { console: BASE_PORT, stats: BASE_PORT + 1, gallery: BASE_PORT + 2 }

export const origins = {
  console: `http://127.0.0.1:${ports.console}`,
  stats: `http://127.0.0.1:${ports.stats}`,
  gallery: `http://127.0.0.1:${ports.gallery}`,
}
