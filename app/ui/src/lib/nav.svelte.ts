/**
 * Page navigation shared between the shell and pages, so a page can send the
 * user somewhere else (for example from Download to the new model).
 */
import type { PageId } from './strings';

export const nav = $state({
  /** The page being shown. */
  page: 'status' as PageId,
  /** A model id the Models page should select next, then clear. */
  modelToSelect: null as string | null,
});

/** Goes to a page. */
export function go(page: PageId): void {
  nav.page = page;
}

/** Opens the Models page on one model. */
export function openModel(id: string): void {
  nav.modelToSelect = id;
  nav.page = 'models';
}
