const root = document.documentElement;
const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
if (!still) root.classList.add('m-enter');

const idea = document.getElementById('idea');
document.getElementById('composer').addEventListener('submit', event => {
  event.preventDefault();
  sessionStorage.setItem('editroom.draft', idea.value.trim());
  document.getElementById('launch-status').textContent = 'Opening studio';
  location.assign('/studio');
});
for (const chip of document.querySelectorAll('[data-prompt]')) {
  chip.addEventListener('click', () => {
    idea.value = chip.dataset.prompt;
    idea.focus();
  });
}

// Entrance order and scroll reveals.
document.querySelectorAll('.panel').forEach((panel, i) => panel.style.setProperty('--i', i));
document.querySelectorAll('.hero-copy > *').forEach((part, i) => part.style.setProperty('--i', i));
const reveal = new IntersectionObserver(entries => {
  for (const entry of entries) {
    if (!entry.isIntersecting) continue;
    entry.target.classList.add('is-in');
    reveal.unobserve(entry.target);
  }
}, {threshold: .12, rootMargin: '0px 0px -6% 0px'});
for (const part of document.querySelectorAll('[data-reveal]')) reveal.observe(part);

// The screening wall plays only while it is on screen, and drifts with the scroll.
const wall = document.getElementById('wall');
const panels = [...wall.querySelectorAll('.panel')];
const quiet = video => video.play().catch(() => {});
if (!still) {
  new IntersectionObserver(([entry]) => {
    for (const panel of panels) {
      const video = panel.querySelector('video');
      if (entry.isIntersecting && panel.offsetParent) quiet(video);
      else video.pause();
    }
  }, {threshold: .05}).observe(wall);
  let queued = false;
  const drift = () => {
    queued = false;
    const y = Math.min(scrollY, innerHeight * 1.2);
    for (const panel of panels) panel.style.setProperty('--shift', `${(y * Number(panel.dataset.depth) * .1).toFixed(1)}px`);
  };
  addEventListener('scroll', () => {
    if (!queued) {
      queued = true;
      requestAnimationFrame(drift);
    }
  }, {passive: true});
}

// Feature presentation.
const feature = document.getElementById('feature-film');
const screen = feature.closest('.screen');
const toggle = document.getElementById('feature-toggle');
const progress = document.getElementById('feature-progress');
const time = document.getElementById('feature-time');
const shots = [...document.querySelectorAll('#feature-shots button')];
const clock = seconds => `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, '0')}`;
let held = still;
toggle.addEventListener('click', () => {
  held = !feature.paused;
  if (held) feature.pause();
  else quiet(feature);
});
feature.addEventListener('play', () => {
  screen.setAttribute('data-playing', '');
  toggle.setAttribute('aria-label', 'Pause Safari');
});
feature.addEventListener('pause', () => {
  screen.removeAttribute('data-playing');
  toggle.setAttribute('aria-label', 'Play Safari');
});
feature.addEventListener('timeupdate', () => {
  const length = feature.duration || 15;
  progress.style.transform = `scaleX(${feature.currentTime / length})`;
  time.textContent = `${clock(feature.currentTime)} / ${clock(length)}`;
  let now = shots[0];
  for (const shot of shots) if (feature.currentTime + .05 >= Number(shot.dataset.at)) now = shot;
  for (const shot of shots) {
    if (shot === now) shot.setAttribute('aria-current', 'true');
    else shot.removeAttribute('aria-current');
  }
});
for (const shot of shots) {
  shot.addEventListener('click', () => {
    feature.currentTime = Number(shot.dataset.at) + .02;
    held = false;
    quiet(feature);
  });
}
new IntersectionObserver(([entry]) => {
  if (entry.isIntersecting && !held) quiet(feature);
  else feature.pause();
}, {threshold: .35}).observe(screen);

// Film library: preview on hover, watch in the theater.
const theater = document.getElementById('theater');
const player = document.getElementById('player');
const fine = matchMedia('(hover: hover) and (pointer: fine)').matches;
for (const film of document.querySelectorAll('.film')) {
  const preview = film.querySelector('video');
  if (fine && !still) {
    film.addEventListener('pointerenter', () => quiet(preview));
    film.addEventListener('pointerleave', () => preview.pause());
    film.addEventListener('focus', () => quiet(preview));
    film.addEventListener('blur', () => preview.pause());
  }
  film.addEventListener('click', () => {
    preview.pause();
    player.poster = film.dataset.poster;
    player.src = film.dataset.film;
    player.setAttribute('aria-label', film.dataset.title);
    document.getElementById('theater-title').textContent = film.dataset.title;
    theater.showModal();
    quiet(player);
  });
}
document.getElementById('theater-close').addEventListener('click', () => theater.close());
theater.addEventListener('click', event => {
  if (event.target === theater) theater.close();
});
theater.addEventListener('close', () => {
  player.pause();
  player.removeAttribute('src');
  player.load();
});

// Studio reel inside the editor screenshot.
const hero = document.getElementById('hero-reel');
const stage = hero.closest('.stage');
const motionButton = document.getElementById('hero-motion');
const motionLabel = document.getElementById('hero-motion-label');
motionButton.addEventListener('click', () => {
  if (!hero.paused) {
    hero.pause();
    return;
  }
  stage.setAttribute('data-reel-open', '');
  hero.play().catch(() => {
    stage.removeAttribute('data-reel-open');
    motionLabel.textContent = 'Retry studio reel';
  });
});
hero.addEventListener('play', () => {
  stage.setAttribute('data-playing', '');
  motionLabel.textContent = 'Pause studio reel';
});
hero.addEventListener('pause', () => {
  stage.removeAttribute('data-playing');
  motionLabel.textContent = hero.currentTime > 0 && !hero.ended ? 'Resume studio reel' : 'Play studio reel';
});
hero.addEventListener('ended', () => {
  stage.removeAttribute('data-reel-open');
  motionLabel.textContent = 'Play studio reel';
});
new IntersectionObserver(([entry]) => {
  if (entry.intersectionRatio < .25) hero.pause();
}, {threshold: .25}).observe(stage);

document.addEventListener('visibilitychange', () => {
  if (!document.hidden) return;
  hero.pause();
  feature.pause();
  for (const panel of panels) panel.querySelector('video').pause();
});
