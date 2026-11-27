'use strict';

const StudioFilmstrip = (() => {
  function tiles({first,last,height,aspect,pps,start,end,duration,sourceDuration,speed,reverse,freeze,ramp,still}) {
    const minWidth = Math.max(still ? 20 : 24,height * aspect);
    const speedForGrid = ramp ? Math.max(.01,(end - start) / duration) : speed;
    let step = 1 / 16;
    while (step / speedForGrid * pps < minWidth && step < 16384) step *= 2;
    const width = ramp || still ? minWidth : step / speed * pps;
    const sourceStart = reverse && !freeze ? sourceDuration - end : start;
    const origin = ramp || still ? 0 : -sourceStart / speed * pps;
    const result = [];
    let index = Math.floor((first - origin) / width);
    for (let left = origin + index * width; left < last && result.length < 240; left = origin + ++index * width) {
      let sample = index * step;
      if (ramp) {
        const offset = ramp(Math.min(duration,(left + width / 2) / pps));
        const time = reverse ? sourceDuration - offset : offset;
        sample = Math.round(time / step) * step;
      }
      if (reverse) sample = sourceDuration - sample;
      const seconds = still ? 0 : Math.max(0,Math.min(sourceDuration - .05,freeze ? start : sample));
      result.push({left,width:Math.max(1,width - (still ? 0 : 1)),ms:Math.round(seconds * 1000)});
    }
    return result;
  }

  class Decoder {
    constructor(finished) {
      this.finished = finished;
      this.player = document.createElement('video');
      this.player.muted = this.player.playsInline = true;
      this.player.preload = 'auto';
      for (const event of ['loadedmetadata','loadeddata','seeked']) {
        this.player.addEventListener(event,() => this.advance());
      }
      this.player.addEventListener('error',() => this.finish(null,null,true));
    }

    load(job) {
      this.job = job;
      this.timer = setTimeout(() => {
        this.releasePlayer();
        this.finish(null);
      },6000);
      if (this.url !== job.url || this.player.error) {
        this.url = job.url;
        this.player.src = job.url;
      }
      this.advance();
    }

    advance() {
      const {player,job} = this;
      if (!job || player.readyState < 1 || player.seeking || !Number.isFinite(player.duration)) return;
      const at = Math.min(job.ms / 1000,Math.max(0,player.duration - .001));
      if (Math.abs(player.currentTime-at) > .0005) {
        player.currentTime = at;
        return;
      }
      if (player.readyState < 2 || !player.videoWidth || !player.videoHeight) return;
      const max = 192;
      const scale = Math.min(1,max / Math.max(player.videoWidth,player.videoHeight));
      const canvas = document.createElement('canvas');
      canvas.width = Math.max(1,Math.round(player.videoWidth * scale));
      canvas.height = Math.max(1,Math.round(player.videoHeight * scale));
      canvas.getContext('2d',{alpha:false}).drawImage(player,0,0,canvas.width,canvas.height);
      this.finish(canvas,player.videoWidth / player.videoHeight);
    }

    finish(frame,aspect,unreadable = false) {
      const job = this.job;
      if (!job) return;
      this.job = null;
      clearTimeout(this.timer);
      this.finished(job,frame,aspect,unreadable);
    }

    releasePlayer() {
      if (!this.url) return;
      this.url = null;
      this.player.removeAttribute('src');
      this.player.load();
    }

    reset() {
      this.job = null;
      clearTimeout(this.timer);
      this.releasePlayer();
    }
  }

  class Frames {
    constructor(redraw) {
      this.redraw = redraw;
      this.cached = new Map();
      this.aspects = new Map();
      this.unreadable = new Set();
      this.decoders = Array.from({length:2},() => new Decoder((job,frame,aspect,unreadable) => {
        this.cached.set(job.key,frame);
        if (aspect) this.aspects.set(job.url,aspect);
        if (unreadable) this.unreadable.add(job.url);
        this.redraw();
      }));
    }

    get(url,ms) {
      const key = `${url}:${ms}`;
      if (!this.cached.has(key)) return undefined;
      const frame = this.cached.get(key);
      this.cached.delete(key);
      this.cached.set(key,frame);
      return frame;
    }

    request(jobs) {
      const desired = [...jobs.values()];
      const busy = new Set(this.decoders.map(decoder => decoder.job?.key));
      for (const decoder of this.decoders) {
        if (decoder.job) continue;
        const available = job => !this.unreadable.has(job.url) && !busy.has(job.key) && !this.cached.has(job.key);
        const job = desired.find(job => job.url === decoder.url && available(job)) || desired.find(available);
        if (job) {
          busy.add(job.key);
          decoder.load(job);
        }
      }
      // Keep all visible frames even if the viewport contains unusually many thumbnails.
      for (const key of this.cached.keys()) {
        if (this.cached.size <= jobs.size + 220) break;
        if (!jobs.has(key)) this.cached.delete(key);
      }
    }

    reset() {
      for (const decoder of this.decoders) decoder.reset();
      this.cached.clear();
      this.aspects.clear();
      this.unreadable.clear();
    }
  }

  return {tiles,Frames};
})();
