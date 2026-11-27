document.documentElement.classList.add('m-enter');
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

const hero = document.getElementById('hero-reel');
const stage = hero.closest('.stage');
const motionButton = document.getElementById('hero-motion');
const motionLabel = document.getElementById('hero-motion-label');
motionButton.addEventListener('click', () => {
  if (!hero.paused) {
    hero.pause();
    return;
  }
  stage.setAttribute('data-reel-open','');
  hero.play().catch(() => {
    stage.removeAttribute('data-reel-open');
    motionLabel.textContent = 'Retry studio reel';
  });
});
hero.addEventListener('play', () => {
  stage.setAttribute('data-playing','');
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
document.addEventListener('visibilitychange', () => {
  if (document.hidden) hero.pause();
});
new IntersectionObserver(([entry]) => {
  if (entry.intersectionRatio<.25) hero.pause();
}, {threshold:.25}).observe(stage);

const player = document.getElementById('player');
const reel = [...document.querySelectorAll('.reel-item')];
for (const item of reel) {
  item.addEventListener('click', () => {
    if (item.getAttribute('aria-pressed') !== 'true') {
      player.pause();
      player.poster = item.dataset.poster;
      player.src = item.dataset.source;
      player.setAttribute('aria-label', item.dataset.title);
      document.getElementById('film-title').textContent = item.dataset.title;
      for (const option of reel) option.setAttribute('aria-pressed', String(item === option));
    }
    player.play().catch(() => {});
  });
}
