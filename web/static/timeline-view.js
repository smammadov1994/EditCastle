'use strict';

const TimelineView = (() => {
  const ruler = 26, zone = 26, gap = 1, divider = 9, caption = 30;
  const minRow = 30, maxRow = 110, barThickness = 15;
  const gripSize = 11, minThumb = gripSize * 2 + 6;
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));

  function rows(height, visible, video, audio, captions) {
    visible = Math.max(40, visible);
    const captionSpace = captions ? caption + gap : 0;
    const content = zone * 2 + divider + captionSpace + (video + audio) * (height + gap);
    const above = captionSpace + zone + video * (height + gap) + divider / 2;
    const world = Math.max(visible, visible * height / minRow, 2 * above, 2 * (content - above));
    return {height, visible, video, audio, captions, content, above, world, top:world / 2 - above};
  }

  function fitRows(visible, video, audio, captions) {
    const room = Math.max(40, visible) + barThickness;
    const fixed = rows(0, visible, video, audio, captions).content - (video + audio) * gap;
    const height = clamp(Math.round((room - fixed - 6) / Math.max(1, video + audio) - gap), minRow, 62);
    const layout = rows(height, visible, video, audio, captions);
    return {layout, scroll:Math.min(0, (layout.content - room) / 2)};
  }

  function rowAnchor(at, layout) {
    const {height, video, audio} = layout;
    const lengths = [[zone, false]];
    for (let i=0; i<video; i++) lengths.push([height, true], [gap, false]);
    lengths.push([divider, false]);
    for (let i=0; i<audio; i++) lengths.push([height, true], [gap, false]);
    lengths.push([zone, false]);
    let fixed = Math.min(0, at), trackRows = 0, position = 0;
    for (const [length, track] of lengths) {
      const part = clamp(at - position, 0, length);
      if (track) trackRows += part / height;
      else fixed += part;
      position += length;
    }
    return {fixed:fixed + Math.max(0, at - position), rows:trackRows};
  }

  function thumb(bar, world, visible, position) {
    const length = visible / world * bar;
    const drawn = Math.min(bar, Math.max(minThumb, length));
    const start = position / world * bar;
    return {start, length, drawn, offset:clamp(start, 0, bar - drawn)};
  }

  function partAt(point, start, length) {
    if (point < start - 2 || point > start + length + 2) return null;
    const grip = Math.min(gripSize + 3, length / 3);
    if (point < start + grip) return 0;
    if (point > start + length - grip) return 2;
    return 1;
  }

  function gripDrag(part, start, end, move, bar, minimum, maximum, mirror = 1) {
    let grow = Math.min(part===2 ? move : -move, part===2 ? bar-end : start);
    const edges = distance => part===2
      ? [Math.max(0, start - distance * mirror), end + distance]
      : [start - distance, Math.min(bar, end + distance * mirror)];
    const fits = distance => {
      const [first, last] = edges(distance);
      return last - first >= minimum - .0001 && last - first <= maximum + .0001;
    };
    if (!fits(grow)) {
      let ok = 0, bad = grow;
      for (let i=0; i<40; i++) {
        const mid = (ok + bad) / 2;
        if (fits(mid)) ok = mid;
        else bad = mid;
      }
      grow = ok;
    }
    return edges(grow);
  }

  function horizontalGrip(part, bar, visible, extent, scale, scroll, playhead, move, maxScale) {
    const position = (playhead * scale - scroll) / Math.max(1, visible);
    let mirror = 1;
    if (part===2 && position>=0 && position<=.5) mirror = position / (1 - position);
    if (part===0 && position>=.5 && position<=1) mirror = (1 - position) / position;
    const {start,length} = thumb(bar, extent * scale, visible, scroll);
    const [first, last] = gripDrag(part, start, start + length, move, bar, visible / (extent * maxScale) * bar, bar, mirror);
    const pixelsPerTick = visible / ((last - first) / bar * extent);
    return {scale:pixelsPerTick, scroll:first / bar * extent * pixelsPerTick};
  }

  function verticalGrip(part, bar, layout, scroll, move) {
    const atHeight = height => rows(height, layout.visible, layout.video, layout.audio, layout.captions);
    const {start,length} = thumb(bar, layout.world, layout.visible, scroll + layout.top);
    const fraction = height => layout.visible / atHeight(height).world * bar;
    const [first,last] = gripDrag(part, start, start + length, move, bar, fraction(maxRow), fraction(minRow));
    let best = layout.height, difference = Infinity;
    for (let height=minRow; height<=maxRow; height++) {
      const miss = Math.abs(fraction(height) - (last - first));
      if (miss < difference) { best = height; difference = miss; }
    }
    const anchor = rowAnchor(scroll + layout.visible / 2, layout);
    return {layout:atHeight(best), scroll:anchor.fixed + anchor.rows * best - layout.visible / 2};
  }

  return {ruler, minRow, maxRow, clamp, rows, fitRows, rowAnchor, thumb, partAt, gripDrag, horizontalGrip, verticalGrip};
})();

if (typeof module !== 'undefined') module.exports = TimelineView;
