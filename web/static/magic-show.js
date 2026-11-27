// The Director show: a thinking overlay while the edit is worked out, then a swarm of cursors
// that performs the finished edit on the timeline. Timings follow Editroom's MagicThinking and MagicShow.
window.MagicShow = (() => {
  const inOut = x => x < .5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2;
  const out = x => 1 - Math.pow(1 - x, 3);
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));
  const between = (low, high) => low + Math.random() * (high - low);
  const lerp = (a, b, u) => a + (b - a) * u;

  function layer(name) {
    const node = document.createElement('div');
    node.className = name;
    node.setAttribute('aria-hidden', 'true');
    document.body.append(node);
    return node;
  }

  // ── Thinking ──────────────────────────────────────────────────────────────
  // options: tint [r,g,b] 0-255, viewer() -> rect, cutouts() -> [{rect, radius, grow}],
  //          targets(stage) -> [{key, rect}]
  function thinking(options) {
    const host = layer('magic-thinking');
    const canvas = document.createElement('canvas');
    host.append(canvas);
    const ink = canvas.getContext('2d');
    const born = performance.now();
    const scans = [];
    const recent = [];
    const queue = [];
    const ghosts = new Map();
    let stage = 'looking';
    let noted = 0;
    let spawned = 0;
    let sketched = 0;
    let dots = 0;
    let last = 0;
    let closing = null;
    let handed = false;
    let bump = -1;
    let frame = 0;
    let gone = false;

    const corners = (rect, size, alpha, gap = 0) => {
      const [x, y, w, h] = [rect.left - gap, rect.top - gap, rect.width + gap * 2, rect.height + gap * 2];
      ink.strokeStyle = `rgba(255,255,255,${.75 * alpha})`;
      ink.lineWidth = 1;
      ink.beginPath();
      for (const [cx, cy, dx, dy] of [[x, y, 1, 1], [x + w, y, -1, 1], [x + w, y + h, -1, -1], [x, y + h, 1, -1]]) {
        ink.moveTo(cx + dx * size, cy);
        ink.lineTo(cx, cy);
        ink.lineTo(cx, cy + dy * size);
      }
      ink.stroke();
    };

    function spawn(now) {
      const all = options.targets(stage).filter(target => target.rect && target.rect.width > 4 && target.rect.height > 4);
      let target = null;
      while (queue.length && !target) {
        const key = queue.shift();
        target = all.find(item => item.key === key);
      }
      if (!target) {
        if (stage === 'thinking' || !all.length) return;
        const limit = Math.min(6, Math.max(1, Math.floor(all.length / 2) + 1));
        if (scans.length >= limit) return;
        const fresh = all.filter(item => !recent.includes(item.key));
        const pool = fresh.length ? fresh : all;
        target = pool[Math.floor(Math.random() * pool.length)];
        recent.push(target.key);
        while (recent.length > Math.floor(all.length * 2 / 3)) recent.shift();
      }
      const {left, top, width, height} = target.rect;
      scans.push({left, top, width, height, start: now, sweep: between(1, 1.5) * clamp(width / 220, .8, 1.6), viewer: target.key === 'viewer'});
      spawned = now;
    }

    function drawDots(rect, now, alpha) {
      const [r, g, b] = options.tint.map(part => lerp(part, 255, .55));
      ink.fillStyle = `rgba(0,0,0,${.22 * alpha})`;
      ink.fillRect(rect.left, rect.top, rect.width, rect.height);
      const gap = Math.max(6, Math.min(rect.width, rect.height) / 55);
      for (let y = gap / 2; y < rect.height; y += gap) {
        for (let x = gap / 2; x < rect.width; x += gap) {
          const a = x / rect.width * 4 + now * .6;
          const c = y / rect.height * 3.2 - now * .44;
          const noise = Math.sin(a * 1.7 + Math.sin(c * 1.3)) * .5 + Math.sin(c * 2.1 - a * .6) * .3 + Math.sin((a + c) * .9) * .2;
          const level = Math.min(1, Math.pow(Math.max(0, noise * .5 + .5), 2.6) * 1.2) * .65;
          ink.fillStyle = `rgba(${Math.round(lerp(r, 255, level * .5))},${Math.round(lerp(g, 255, level * .4))},${Math.round(lerp(b, 255, level * .3))},${(.05 + .6 * level) * alpha})`;
          ink.beginPath();
          ink.arc(rect.left + x, rect.top + y, .35 + level * gap * .16, 0, 6.2832);
          ink.fill();
        }
      }
    }

    function drawGhost(ghost, now) {
      const rect = ghost.rect();
      if (!rect || rect.width < 3) return;
      const fade = ghost.landed == null ? 1 : 1 - (now - ghost.landed) / .3;
      if (fade <= 0) return;
      const drawn = clamp((now - ghost.start) / .7, 0, 1);
      if (drawn <= 0) return;
      const [x, y, w, h] = [rect.left + 1, rect.top + 3, rect.width - 2, rect.height - 6];
      const radius = Math.min(6, w / 2, h / 2);
      if (drawn > .6) {
        ink.fillStyle = `rgba(255,255,255,${.06 * (drawn - .6) / .4 * fade})`;
        ink.beginPath();
        ink.roundRect(x, y, w, h, radius);
        ink.fill();
      }
      const around = 2 * (w + h);
      ink.save();
      ink.beginPath();
      ink.roundRect(x, y, w, h, radius);
      ink.strokeStyle = `rgba(255,255,255,${.65 * fade})`;
      ink.lineWidth = 1.5;
      if (drawn < 1) {
        // The outline draws on from the top-left corner, then its dashes start marching.
        ink.setLineDash([around * drawn, around]);
      } else {
        ink.setLineDash([7, 6]);
        ink.lineDashOffset = now * 24;
      }
      ink.stroke();
      ink.restore();
    }

    function draw(time) {
      if (gone) return;
      frame = requestAnimationFrame(draw);
      const now = (time - born) / 1000;
      const step = Math.min(.1, now - last);
      last = now;
      const scale = devicePixelRatio || 1;
      const [width, height] = [innerWidth, innerHeight];
      if (canvas.width !== Math.round(width * scale) || canvas.height !== Math.round(height * scale)) {
        canvas.width = Math.round(width * scale);
        canvas.height = Math.round(height * scale);
      }
      ink.setTransform(scale, 0, 0, scale, 0, 0);
      ink.clearRect(0, 0, width, height);
      const fade = closing == null ? 1 : clamp(1 - (now - closing) / .6, 0, 1);

      // Dim, with the Director panel and its button left clear. The ghosts are drawn after it, so
      // the sketch of the coming cut stays bright.
      ink.fillStyle = `rgba(0,0,0,${.45 * Math.min(1, now / .35) * fade})`;
      ink.beginPath();
      ink.rect(0, 0, width, height);
      for (const hole of options.cutouts()) {
        const grow = hole.grow || 0;
        const rect = hole.rect;
        if (!rect || !rect.width) continue;
        ink.roundRect(rect.left - grow, rect.top - grow, rect.width + grow * 2, rect.height + grow * 2,
          Math.min(hole.radius + grow, (rect.height + grow * 2) / 2));
      }
      ink.fill('evenodd');

      const viewer = options.viewer();
      const wanted = stage === 'looking' || closing != null ? 0 : 1;
      dots += (wanted - dots) * Math.min(1, step * (wanted ? 1.2 : 3));
      if (viewer && viewer.width > 20) {
        if (dots > .01) drawDots(viewer, now, dots * fade);
        corners(viewer, 6, fade, 1.5);
      }

      if (closing == null && now > .25 && now - spawned > .18) spawn(now);
      if (stage === 'analyzing' && now - noted > 2.5 && !queue.length) stage = 'thinking';
      for (let index = scans.length - 1; index >= 0; index--) {
        const scan = scans[index];
        const age = now - scan.start;
        if (age > scan.sweep + .55) {
          scans.splice(index, 1);
          continue;
        }
        const sweep = Math.min(1, age / scan.sweep);
        const box = age < scan.sweep ? 1 : clamp(1 - (age - scan.sweep) / .3, 0, 1);
        ink.fillStyle = `rgba(255,255,255,${.07 * box * fade})`;
        ink.fillRect(scan.left, scan.top, scan.width * sweep, scan.height);
        ink.fillStyle = `rgba(255,255,255,${.8 * box * fade})`;
        ink.fillRect(scan.left + scan.width * sweep - 1.5, scan.top, 1.5, scan.height);
        const marks = Math.min(1, age / .15) * (age < scan.sweep + .25 ? 1 : clamp(1 - (age - scan.sweep - .25) / .3, 0, 1));
        corners(scan, scan.viewer ? 6 : 3, marks * fade);
      }

      for (const [key, ghost] of ghosts) {
        if (ghost.landed != null && now - ghost.landed > .3) ghosts.delete(key);
        else if (!handed && closing != null) ghost.landed ??= closing + .3;
        drawGhost(ghost, now);
      }

      // Hint.
      const shown = clamp((now - .6) / .4, 0, 1) * fade;
      if (shown > 0) {
        const nudge = bump < 0 ? 0 : clamp((now - bump) / .45, 0, 1);
        const grow = 1 + .08 * Math.sin(nudge * Math.PI) * (nudge < 1 ? 1 : 0);
        ink.save();
        ink.translate(width / 2, height - 26);
        ink.scale(grow, grow);
        ink.fillStyle = `rgba(0,0,0,${.7 * shown})`;
        ink.beginPath();
        ink.roundRect(-70, -11, 140, 22, 11);
        ink.fill();
        ink.fillStyle = `rgba(255,255,255,${(nudge > 0 && nudge < 1 ? 1 : .75) * shown})`;
        ink.font = '500 11px system-ui, sans-serif';
        ink.textAlign = 'center';
        ink.textBaseline = 'middle';
        ink.fillText('Press esc to cancel', 0, .5);
        ink.restore();
      }

      if (closing != null && now - closing > .6 && !ghosts.size) remove();
    }

    function remove() {
      if (gone) return;
      gone = true;
      cancelAnimationFrame(frame);
      removeEventListener('pointerdown', blocked, true);
      host.remove();
    }
    const clock = () => (performance.now() - born) / 1000;
    const blocked = event => {
      if (closing != null) return;
      const inside = options.cutouts().some(hole => hole.rect && event.clientX >= hole.rect.left && event.clientX <= hole.rect.right &&
        event.clientY >= hole.rect.top && event.clientY <= hole.rect.bottom);
      if (!inside) bump = clock();
    };
    addEventListener('pointerdown', blocked, true);
    frame = requestAnimationFrame(draw);

    return {
      get stage() { return stage; },
      // Stages only move forward: looking, analyzing, thinking.
      advance(next) {
        const order = ['looking', 'analyzing', 'thinking'];
        if (order.indexOf(next) > order.indexOf(stage)) stage = next;
        noted = clock();
      },
      focus(key) {
        noted = clock();
        if (!queue.includes(key)) queue.push(key);
      },
      plan(key, rect) {
        const now = clock();
        const known = ghosts.get(key);
        if (known) {
          known.rect = rect;
          return;
        }
        sketched = Math.max(now, sketched + .12);
        ghosts.set(key, {rect, start: sketched, landed: null});
      },
      landed(key) {
        const ghost = ghosts.get(key);
        if (ghost && ghost.landed == null) ghost.landed = clock();
      },
      get drawnIn() { return Math.max(0, sketched + .7 - clock()); },
      // The swarm takes over: the dim, scans and dots fade, each ghost waits for its clip.
      handOver() {
        handed = true;
        closing ??= clock();
      },
      release() {
        const now = clock();
        for (const ghost of ghosts.values()) ghost.landed ??= now;
        closing ??= now;
      },
      end() {
        handed = false;
        closing ??= clock();
      },
      remove
    };
  }

  // ── Swarm ─────────────────────────────────────────────────────────────────
  const performTime = {place: .5, trim: .28, move: .42, blade: .16, remove: .14, change: .16, gain: .24, fade: .22, look: .1, grade: .2, transition: .46};

  function arrow(color) {
    const space = 'http://www.w3.org/2000/svg';
    const picture = document.createElementNS(space, 'svg');
    picture.setAttribute('viewBox', '0 0 18 24');
    picture.setAttribute('width', '18');
    picture.setAttribute('height', '24');
    const shape = document.createElementNS(space, 'polygon');
    shape.setAttribute('points', '2,2 2,18.5 6.2,14.6 9.1,21 11.6,19.9 8.8,13.6 14.4,13.6');
    shape.setAttribute('fill', color);
    shape.setAttribute('stroke', '#fff');
    shape.setAttribute('stroke-width', '1.3');
    shape.setAttribute('stroke-linejoin', 'round');
    picture.append(shape);
    return picture;
  }

  const bezier = (from, bend, to, u) => ({
    x: (1 - u) * (1 - u) * from.x + 2 * (1 - u) * u * bend.x + u * u * to.x,
    y: (1 - u) * (1 - u) * from.y + 2 * (1 - u) * u * bend.y + u * u * to.y
  });
  const onScreen = point => ({x: clamp(point.x, 12, innerWidth - 12), y: clamp(point.y, 40, innerHeight - 12)});

  // options: tasks [{kind, label, delay, points() -> {grab, release}, begin(), apply(u), finish(), cool(),
  //          carry: {from: {width, height}, to: {width, height}, image}}], origin() -> {x, y}
  function swarm(options) {
    const host = layer('magic-swarm');
    const tasks = options.tasks;
    const home = options.origin();
    const few = tasks.length <= 6;
    const count = Math.min(64, Math.max(14, Math.floor(tasks.length / 2.4)));
    const hands = Array.from({length: count}, (_, index) => ({
      index,
      hue: (index * .61803398875) % 1,
      speed: index < 2 ? between(1, 1.35) : between(.65, 1.35),
      angle: between(0, Math.PI * 2),
      birth: Math.max(0, 2.4 * Math.pow(index / count, .7) + between(-.06, .06)),
      free: 0,
      at: home,
      slots: []
    }));
    for (const hand of hands) hand.free = hand.birth;

    // Each task waits for the task it follows (a fade follows its clip's placement), then goes to
    // the cursor that can reach it soonest. A task marked `same` keeps the cursor of the one before.
    const pace = few ? .85 : 1;
    const waiting = tasks.map(task => ({task, think: Math.pow(Math.random(), 1.5) * .45, spots: task.points()})).filter(entry => entry.spots);
    const known = new Set(waiting.map(entry => entry.task));
    const placed = new Map();
    while (waiting.length) {
      let pick = -1;
      let ready = Infinity;
      waiting.forEach((entry, index) => {
        const lead = known.has(entry.task.after) ? entry.task.after : null;
        if (lead && !placed.has(lead)) return;
        const at = (lead ? placed.get(lead).end : 0) + (entry.task.delay || 0) + (entry.task.same ? 0 : entry.think);
        if (at < ready) [pick, ready] = [index, at];
      });
      if (pick < 0) break;
      const [entry] = waiting.splice(pick, 1);
      const lead = entry.task.same && placed.get(entry.task.after);
      let best = null;
      for (const hand of lead ? [lead.hand] : hands) {
        const distance = Math.hypot(entry.spots.grab.x - hand.at.x, entry.spots.grab.y - hand.at.y);
        const travel = clamp(distance / (2200 * hand.speed / pace), .14, .65);
        const arrive = hand.free + travel;
        // A cursor already out wins a tie, so the swarm does not send a new one for every edit.
        const score = arrive - (hand.slots.length ? .0001 : 0);
        if (!best || score < best.score) best = {hand, travel, arrive, score};
      }
      const {hand, travel, arrive} = best;
      const start = Math.max(ready, arrive);
      const length = entry.task.seconds ?? (performTime[entry.task.kind] || .16) * pace * (entry.task.steady ? between(.85, 1.15) : between(.7, 1.5));
      const slot = {task: entry.task, hand, start, travel, end: start + length, begun: false, done: false, cooled: false, near: false};
      hand.slots.push(slot);
      placed.set(entry.task, slot);
      hand.free = start + length + .03;
      hand.at = entry.spots.release;
    }
    const working = hands.filter(hand => hand.slots.length);
    working.forEach((hand, order) => {
      // Cursors leave the button one by one, never later than their first edit needs them.
      hand.birth = Math.min(Math.max(0, 2.4 * Math.pow(order / working.length, .7) + between(-.06, .06)),
        Math.max(0, hand.slots[0].start - hand.slots[0].travel));
      hand.leave = hand.slots.at(-1).end + .08 + between(0, .12);
      hand.color = `hsl(${Math.round(hand.hue * 360)} 92% 64%)`;
      hand.node = document.createElement('div');
      hand.node.className = 'magic-cursor';
      hand.node.style.setProperty('--magic-color', hand.color);
      hand.tag = document.createElement('b');
      hand.node.append(arrow(hand.color), hand.tag);
      hand.node.hidden = true;
      host.append(hand.node);
      hand.rest = home;
    });
    const total = Math.max(0, ...working.map(hand => hand.leave + .42));
    const quick = Math.min(total, 1.2);
    const rate = !few && total > quick + 7 ? (total - quick) / 7 : 1;
    const began = performance.now();
    let frame = 0;
    let settled;
    let over = false;
    const done = new Promise(resolve => { settled = resolve; });

    function carry(hand, slot, point, u) {
      const item = slot.task.carry;
      if (!item) return;
      if (!hand.load) {
        hand.load = document.createElement('i');
        hand.load.className = 'magic-carry';
        hand.load.style.borderColor = hand.color;
        if (item.image) hand.load.style.backgroundImage = `url("${item.image}")`;
        host.append(hand.load);
      }
      const width = lerp(item.from.width, item.to.width, u);
      const height = lerp(item.from.height, item.to.height, u);
      hand.load.style.width = width + 'px';
      hand.load.style.height = height + 'px';
      hand.load.style.transform = `translate(${point.x - .3 * width}px,${point.y - height / 2}px)`;
    }
    function drop(hand) {
      hand.load?.remove();
      hand.load = null;
    }
    function settle(slot) {
      if (!slot.near) {
        slot.near = true;
        slot.task.approach?.();
      }
      if (!slot.begun) {
        slot.begun = true;
        slot.task.begin?.();
      }
      if (!slot.done) {
        slot.done = true;
        slot.task.apply?.(1);
        slot.task.progress?.(1);
        slot.task.finish?.();
      }
    }

    const named = (slot, raw) => typeof slot.task.label === 'function' ? slot.task.label(raw) : slot.task.label;
    function place(hand, now) {
      const node = hand.node;
      if (now < hand.birth) {
        node.hidden = true;
        return true;
      }
      let point = hand.rest;
      let size = 1;
      let opacity = 1;
      let label = '';
      const pop = clamp((now - hand.birth) / .12, 0, 1);
      size *= lerp(.45, 1, out(pop));
      opacity *= Math.min(1, pop * 2);
      const index = hand.slots.findIndex(slot => now < slot.end);
      for (const slot of hand.slots) {
        if (now >= slot.end) settle(slot);
        if (slot.done && !slot.cooled && now >= slot.end + .12) {
          slot.cooled = true;
          slot.task.cool?.();
        }
      }
      if (index < 0) {
        const last = hand.slots.at(-1);
        drop(hand);
        if (now < hand.leave) {
          if (now < last.end + .12) label = named(last, 1);
        } else {
          const u = clamp((now - hand.leave) / .42, 0, 1);
          if (u >= 1) {
            node.hidden = true;
            return false;
          }
          const bend = onScreen({x: (hand.rest.x + home.x) / 2 + Math.cos(hand.angle) * 120, y: (hand.rest.y + home.y) / 2 + Math.sin(hand.angle) * 120});
          point = bezier(hand.rest, bend, home, inOut(u));
          size *= 1 - .5 * out(u);
          opacity *= u > .6 ? 1 - (u - .6) / .4 : 1;
        }
      } else {
        const slot = hand.slots[index];
        const spots = slot.done ? null : slot.task.points();
        if (spots) slot.spots = spots;
        const {grab, release} = slot.spots || {grab: hand.rest, release: hand.rest};
        const depart = Math.max(index ? hand.slots[index - 1].end + .03 : hand.birth, slot.start - slot.travel);
        if (now < depart) {
          if (index) {
            const sway = now * 6 + 3 * hand.angle;
            point = {x: hand.rest.x + Math.sin(sway) * 3, y: hand.rest.y + Math.cos(.8 * sway) * 3};
          }
        } else if (now < slot.start) {
          if (!slot.near) {
            slot.near = true;
            slot.task.approach?.();
          }
          const u = (now - depart) / (slot.start - depart);
          const [dx, dy] = [grab.x - hand.rest.x, grab.y - hand.rest.y];
          let bend;
          if (index) {
            const side = hand.index % 2 ? .16 : -.16;
            bend = {x: hand.rest.x + dx / 2 - dy * side, y: hand.rest.y + dy / 2 + dx * side};
          } else {
            const turn = (hand.angle > Math.PI ? .6 : -.6) * (.6 + .4 * Math.sin(3 * hand.angle));
            bend = {x: hand.rest.x + (dx * Math.cos(turn) - dy * Math.sin(turn)) * .45, y: hand.rest.y + (dx * Math.sin(turn) + dy * Math.cos(turn)) * .45};
          }
          point = bezier(hand.rest, onScreen(bend), grab, inOut(u));
          if (slot.start - now < .14) label = named(slot, 0);
        } else {
          if (!slot.begun) {
            slot.begun = true;
            slot.task.begin?.();
          }
          const raw = clamp((now - slot.start) / (slot.end - slot.start), 0, 1);
          const u = inOut(raw);
          point = slot.task.position ? slot.task.position(raw, grab, release) : {x: lerp(grab.x, release.x, u), y: lerp(grab.y, release.y, u)};
          slot.task.apply?.(u);
          slot.task.progress?.(raw);
          carry(hand, slot, point, u);
          size *= .88;
          label = named(slot, raw);
          hand.rest = release;
        }
        if (now < slot.start) drop(hand);
      }
      node.hidden = false;
      node.style.opacity = opacity;
      node.style.transform = `translate(${point.x}px,${point.y}px)`;
      node.firstChild.style.transform = `scale(${size})`;
      if (hand.tag.textContent !== label) hand.tag.textContent = label;
      hand.tag.hidden = !label;
      return true;
    }

    function finish() {
      if (over) return;
      over = true;
      cancelAnimationFrame(frame);
      for (const hand of working) for (const slot of hand.slots) {
        settle(slot);
        if (!slot.cooled) slot.task.cool?.();
      }
      host.remove();
      settled();
    }
    function tick(time) {
      if (over) return;
      const real = (time - began) / 1000;
      const now = real <= quick ? real : quick + (real - quick) * rate;
      let alive = false;
      for (const hand of working) alive = place(hand, now) || alive;
      if (alive) frame = requestAnimationFrame(tick);
      else finish();
    }
    if (working.length) frame = requestAnimationFrame(tick);
    else finish();
    return {done, skip: finish, count: working.length};
  }

  return {thinking, swarm, arrow};
})();
