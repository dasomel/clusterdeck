import type { Host } from '../api/tauri';

export function gib(bytes: number | null): string {
  if (bytes == null) return '—';
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}

export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KB`;
}

export function formatNum(n: number | null, suffix = ''): string {
  if (n == null) return '—';
  return `${Number(n.toFixed(1))}${suffix}`;
}

export function formatDisk(diskGib: number | null, diskUsedGib: number | null): string {
  if (diskGib == null) return '—';
  if (diskUsedGib != null) {
    return `${formatNum(diskUsedGib)} / ${formatNum(diskGib)} GiB`;
  }
  return `${formatNum(diskGib)} GiB`;
}

export function count(n: number, singular: string, plural?: string): string {
  return `${n} ${n === 1 ? singular : (plural ?? `${singular}s`)}`;
}

export function dateShort(dateStr: string | null): string {
  if (!dateStr) return '—';
  try {
    const d = new Date(dateStr);
    return isNaN(d.getTime()) ? dateStr : d.toLocaleDateString();
  } catch {
    return dateStr;
  }
}

export function formatDateMs(ms: number | string | null): string {
  if (ms == null) return '—';
  try {
    const d = new Date(Number(ms));
    return isNaN(d.getTime()) ? '—' : d.toLocaleDateString();
  } catch {
    return '—';
  }
}

export function formatDateTimeTooltip(ms: number | string | null): string | undefined {
  if (ms == null) return undefined;
  try {
    const d = new Date(Number(ms));
    return isNaN(d.getTime()) ? undefined : d.toLocaleString();
  } catch {
    return undefined;
  }
}

export function formatTimeOnly(timestamp: string | number | null): string {
  if (!timestamp) return '—';
  try {
    const num = typeof timestamp === 'string' ? Number(timestamp) : timestamp;
    const d = isNaN(num) ? new Date(timestamp) : new Date(num);
    return isNaN(d.getTime()) ? '—' : d.toLocaleTimeString();
  } catch {
    return '—';
  }
}

export function relativeTime(dateStr: string | null): string {
  if (!dateStr) return '—';
  try {
    const d = new Date(dateStr);
    if (isNaN(d.getTime())) return dateStr;
    const diffSec = Math.floor((Date.now() - d.getTime()) / 1000);
    if (diffSec < 60) return 'just now';
    if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
    if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
    return `${Math.floor(diffSec / 86400)}d ago`;
  } catch {
    return dateStr;
  }
}

export function target(host: Pick<Host, 'name' | 'address' | 'port'>): string {
  return `${host.name} (${host.address}:${host.port})`;
}
