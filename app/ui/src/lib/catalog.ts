/**
 * The parameter catalog (about a hundred settings) is large to serialise and
 * changes only when the engine does, so pages share one fetched copy.
 */
import { api } from './api';
import type { ParamCatalog } from './types';

let cached: Promise<ParamCatalog> | null = null;

/** The catalog, fetched on first use. */
export function paramCatalog(): Promise<ParamCatalog> {
  cached ??= api.getParamCatalog().catch((e) => {
    cached = null; // do not remember a failure
    throw e;
  });
  return cached;
}

/** Forgets the copy; call after the engine changed. */
export function invalidateCatalog(): void {
  cached = null;
}

/** Asks the core to read the catalog from the engine again, and remembers the result. */
export async function regenerateCatalog(): Promise<ParamCatalog> {
  const fresh = await api.refreshCatalog();
  cached = Promise.resolve(fresh);
  return fresh;
}
