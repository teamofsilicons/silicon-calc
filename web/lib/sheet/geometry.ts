/**
 * One axis of the grid (rows or columns): how big each position is, where it starts, and which position sits at a
 * given offset. Most positions have the default size, so the axis keeps only the exceptions (a resized or hidden
 * position) sorted, with prefix sums of how much each one adds or takes away. Every lookup is a binary search:
 * a million rows cost nothing until someone resizes them, and offset ↔ index stays O(log n).
 */

export class AxisSizes {
  /** How many positions the axis has. */
  count: number;
  readonly defaultSize: number;
  /** Explicit sizes (px), by position. */
  private sizes = new Map<number, number>();
  private hidden = new Set<number>();
  /** Positions with an exception, sorted, and the running sum of (effective size - default) up to and including each. */
  private keys: number[] = [];
  private deltas: number[] = [];
  private dirty = false;

  constructor(defaultSize: number, count: number) {
    this.defaultSize = defaultSize;
    this.count = Math.max(0, Math.floor(count));
  }

  /** Sets one position's size (null: back to the default). */
  setSize(index: number, size: number | null): void {
    if (size === null || size === this.defaultSize) this.sizes.delete(index);
    else this.sizes.set(index, Math.max(2, size));
    this.dirty = true;
  }

  setHidden(index: number, hidden: boolean): void {
    if (hidden) this.hidden.add(index);
    else this.hidden.delete(index);
    this.dirty = true;
  }

  /** Replaces every exception at once (an answer from the service). */
  reset(sizes: Iterable<[number, number]>, hidden: Iterable<number>): void {
    this.sizes.clear();
    this.hidden.clear();
    for (const [index, size] of sizes) if (size !== this.defaultSize) this.sizes.set(index, Math.max(2, size));
    for (const index of hidden) this.hidden.add(index);
    this.dirty = true;
  }

  isHidden(index: number): boolean {
    return this.hidden.has(index);
  }

  /** The size set for a position, ignoring whether it is hidden. */
  explicitSize(index: number): number {
    return this.sizes.get(index) ?? this.defaultSize;
  }

  hiddenPositions(): number[] {
    return [...this.hidden].sort((a, b) => a - b);
  }

  sizedPositions(): Array<[number, number]> {
    return [...this.sizes.entries()].sort((a, b) => a[0] - b[0]);
  }

  private rebuild(): void {
    if (!this.dirty) return;
    const all = new Set<number>([...this.sizes.keys(), ...this.hidden]);
    this.keys = [...all].filter(index => index >= 0).sort((a, b) => a - b);
    this.deltas = new Array(this.keys.length);
    let sum = 0;
    for (let i = 0; i < this.keys.length; i++) {
      const index = this.keys[i]!;
      sum += this.sizeOf(index) - this.defaultSize;
      this.deltas[i] = sum;
    }
    this.dirty = false;
  }

  /** The size of a position as drawn (0 when hidden). */
  sizeOf(index: number): number {
    if (this.hidden.has(index)) return 0;
    return this.sizes.get(index) ?? this.defaultSize;
  }

  /** How many exceptions lie before `index` (binary search). */
  private before(index: number): number {
    let lo = 0;
    let hi = this.keys.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.keys[mid]! < index) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  /** Where a position starts: the sum of the sizes of every position before it. */
  offsetOf(index: number): number {
    this.rebuild();
    const i = Math.max(0, Math.min(index, this.count));
    const n = this.before(i);
    return i * this.defaultSize + (n > 0 ? this.deltas[n - 1]! : 0);
  }

  /** The whole axis' length. */
  total(): number {
    return this.offsetOf(this.count);
  }

  /**
   * The position whose span holds `offset` (the last visible one at or before it for an offset past a hidden run),
   * clamped to the axis. Offsets below 0 give 0; past the end, the last position.
   */
  indexAt(offset: number): number {
    if (this.count === 0) return 0;
    if (offset <= 0) return this.firstVisibleFrom(0);
    this.rebuild();
    // The largest index whose start is <= offset.
    let lo = 0;
    let hi = this.count - 1;
    while (lo < hi) {
      const mid = Math.ceil((lo + hi) / 2);
      if (this.offsetOf(mid) <= offset) lo = mid;
      else hi = mid - 1;
    }
    // Hidden positions have no span: step back to the visible one this offset falls in.
    let index = lo;
    while (index > 0 && this.sizeOf(index) === 0) index--;
    return index;
  }

  /** The first position at or after `index` that is not hidden (or the last position). */
  firstVisibleFrom(index: number): number {
    let i = Math.max(0, index);
    while (i < this.count - 1 && this.hidden.has(i)) i++;
    return i;
  }

  /** The nearest visible position walking from `index` in `step` (+1 or -1); stays put at the edge. */
  nextVisible(index: number, step: 1 | -1): number {
    let i = index + step;
    while (i >= 0 && i < this.count && this.hidden.has(i)) i += step;
    if (i < 0 || i >= this.count) return index;
    return i;
  }

  /** Inserts `n` default-sized positions at `at`, moving the exceptions after it. */
  insert(at: number, n: number): void {
    const move = <T,>(map: Map<number, T>) => {
      const entries = [...map.entries()];
      map.clear();
      for (const [index, value] of entries) map.set(index >= at ? index + n : index, value);
    };
    move(this.sizes);
    const hidden = [...this.hidden];
    this.hidden = new Set(hidden.map(index => (index >= at ? index + n : index)));
    this.count += n;
    this.dirty = true;
  }

  /** Removes `n` positions from `at`. */
  remove(at: number, n: number): void {
    const move = <T,>(map: Map<number, T>) => {
      const entries = [...map.entries()];
      map.clear();
      for (const [index, value] of entries) {
        if (index >= at && index < at + n) continue;
        map.set(index >= at + n ? index - n : index, value);
      }
    };
    move(this.sizes);
    const hidden = [...this.hidden].filter(index => index < at || index >= at + n);
    this.hidden = new Set(hidden.map(index => (index >= at + n ? index - n : index)));
    this.count = Math.max(0, this.count - n);
    this.dirty = true;
  }
}

/**
 * Browsers cap an element's size (Chrome near 33.5 million px, Firefox near 17.9 million). A tab of ten million rows is
 * far taller, so the scroll container's spacer is capped and scrolling maps onto the content proportionally: the
 * scrollbar spans the whole tab, and each pixel of scroll is a little more than one pixel of content once past the cap.
 */
export const MAX_SCROLL_PX = 12_000_000;

export interface ScrollMap {
  /** The spacer's size (px). */
  domSize: number;
  /** Content px per scroll px (1 below the cap). */
  ratio: number;
}

/** How a scrollable content length maps onto a capped scroll range, for a viewport of `view` px. */
export function scrollMap(content: number, view: number): ScrollMap {
  if (content <= MAX_SCROLL_PX) return { domSize: content, ratio: 1 };
  const range = Math.max(1, content - view);
  const domRange = Math.max(1, MAX_SCROLL_PX - view);
  return { domSize: MAX_SCROLL_PX, ratio: range / domRange };
}

/** Content offset for a DOM scroll position. */
export const toContent = (map: ScrollMap, scroll: number) => scroll * map.ratio;
/** DOM scroll position for a content offset. */
export const toScroll = (map: ScrollMap, offset: number) => offset / map.ratio;
