import type { ProblemView } from "./api";

export function problemPage(items: ProblemView[], query: string, requestedPage: number) {
  const search = query.trim().toLowerCase();
  const filtered = search ? items.filter((item) =>
    [item.name ?? "Unknown sticker", item.station_name, item.message]
      .some((value) => value.toLowerCase().includes(search)),
  ) : items;
  const pages = Math.max(1, Math.ceil(filtered.length / 25));
  const page = Math.max(0, Math.min(requestedPage, pages - 1));
  const start = page * 25;
  return { rows: filtered.slice(start, start + 25), total: filtered.length, page, pages, start };
}
