/**
 * A ref cleanup that moves focus to the target when the element leaves the page holding it,
 * once the page has updated and only if nothing else took focus.
 */
export function handOffFocus(
  element: HTMLElement | null,
  target: () => HTMLElement | null | undefined,
): () => void {
  return () => {
    if (element?.contains(document.activeElement)) {
      queueMicrotask(() => {
        if (document.activeElement === document.body) {
          target()?.focus();
        }
      });
    }
  };
}
