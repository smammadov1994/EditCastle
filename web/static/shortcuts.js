'use strict';

const StudioShortcuts = (() => {
  const key = (key, {command=false, shift=false, option=false, control=false}={}) =>
    ({key,command,shift,option,control});
  const cmd = (value, modifiers={}) => key(value,{...modifiers,command:true});
  const shift = value => key(value,{shift:true});
  const option = value => key(value,{option:true});
  const groups = {
    File: [
      ['new','New',cmd('n')], ['open','Open…',cmd('o')], ['save','Save',cmd('s')],
      ['saveAs','Save As…',cmd('s',{shift:true})],
      ['importMedia','Import Media…',cmd('i')], ['export','Export…',cmd('e')]
    ],
    Edit: [
      ['undo','Undo',cmd('z')], ['redo','Redo',cmd('z',{shift:true})]
    ],
    Playback: [
      ['playPause','Play / Pause',key('space')], ['stepBack','Back One Frame',key('left')],
      ['stepForward','Forward One Frame',key('right')],
      ['previousEdit','Previous Edit',key('up'),shift('left')],
      ['nextEdit','Next Edit',key('down'),shift('right')],
      ['goToStart','Go to Start',key('home')], ['goToEnd','Go to End',key('end')],
      ['loop','Loop Playback',key('l')], ['fullScreenViewer','Full Screen Viewer',key('f')]
    ],
    Tools: [['selectTool','Select Tool',key('v'),key('a')], ['cutTool','Cut Tool',key('c')]],
    Clip: [
      ['split','Split at Playhead',cmd('k')], ['addDissolve','Add Dissolve',cmd('d')],
      ['detachAudio','Detach Audio',cmd('l',{shift:true})], ['duplicate','Duplicate',cmd('d',{shift:true})],
      ['freezeFrame','Freeze Frame',option('f')], ['reverse','Reverse',option('r')],
      ['copyGrade','Copy Grade',cmd('c',{option:true})], ['pasteGrade','Paste Grade',cmd('v',{option:true})],
      ['delete','Delete',key('delete'),key('forwardDelete')],
      ['rippleDelete','Ripple Delete',shift('delete'),shift('forwardDelete')],
      ['rippleTrimStart','Ripple Trim Start to Playhead',key('q')],
      ['rippleTrimEnd','Ripple Trim End to Playhead',key('w')], ['addText','Add Text',cmd('t')]
    ],
    Mark: [
      ['addMarker','Add Marker',key('m')], ['setIn','Set In',key('i')],
      ['setOut','Set Out',key('o')], ['clearInOut','Clear In and Out',shift('x')]
    ],
    View: [
      ['zoomIn','Zoom In',cmd('='),key('='),shift('+')],
      ['zoomOut','Zoom Out',cmd('-'),key('-')],
      ['zoomToFit','Zoom to Fit',cmd('0'),shift('z'),key('\\')],
      ['snapping','Snapping',key('s')], ['sticky','Sticky Edges',key('r')],
      ['skimming','Skimming',key('k')], ['audioScrubbing','Audio Scrubbing',shift('s')]
    ]
  };
  const definitions = Object.entries(groups).flatMap(([group,rows])=>
    rows.map(([id,title,...defaults])=>({id,title,group,defaults})));
  const named = { ' ':'space', Backspace:'delete',Delete:'forwardDelete',ArrowLeft:'left',
    ArrowRight:'right',ArrowUp:'up',ArrowDown:'down',Home:'home',End:'end',
    Enter:'return',Tab:'tab',PageUp:'pageUp',PageDown:'pageDown' };
  const physical = {Minus:'-',Equal:'=',Backslash:'\\',BracketLeft:'[',BracketRight:']',
    Comma:',',Period:'.',Slash:'/',Semicolon:';',Quote:"'",Backquote:'`'};
  const glyphs = {space:'Space',delete:'⌫',forwardDelete:'⌦',left:'←',right:'→',up:'↑',down:'↓',
    home:'Home',end:'End',return:'↩',tab:'⇥',pageUp:'Page Up',pageDown:'Page Down'};
  const preferenceKey = 'editroom.shortcuts';
  const skimKey = 'editroom.play-from-skimmer';

  function fromEvent(event) {
    if (event.isComposing || event.key==='Escape' || event.getModifierState?.('AltGraph')) return null;
    let value = named[event.key];
    if (!value && event.altKey) {
      if (/^Key[A-Z]$/.test(event.code)) value = event.code.at(-1).toLowerCase();
      else if (/^Digit[0-9]$/.test(event.code)) value = event.code.at(-1);
      else value = physical[event.code];
    }
    if (!value && ([...event.key].length===1 || /^F(?:[1-9]|1[0-9]|2[0-4])$/.test(event.key))) {
      value = event.key.toLowerCase();
    }
    return value ? key(value,{command:event.metaKey,shift:event.shiftKey,option:event.altKey,control:event.ctrlKey}) : null;
  }

  function equal(a,b) {
    return a.key===b.key && a.command===b.command && a.shift===b.shift &&
      a.option===b.option && a.control===b.control;
  }
  function display(combo) {
    return (combo.control?'⌃':'')+(combo.option?'⌥':'')+(combo.shift?'⇧':'')+
      (combo.command?'⌘':'')+(glyphs[combo.key] || combo.key.toUpperCase());
  }
  function keyboardShortcut(combo) {
    const special = {'space':'Space','delete':'Backspace','forwardDelete':'Delete',
      left:'ArrowLeft',right:'ArrowRight',up:'ArrowUp',down:'ArrowDown',return:'Enter',
      pageUp:'PageUp',pageDown:'PageDown',home:'Home',end:'End',tab:'Tab'};
    return [...(combo.control?['Control']:[]),...(combo.option?['Alt']:[]),...(combo.shift?['Shift']:[]),
      ...(combo.command?['Meta']:[]),special[combo.key] || (combo.key==='+'?'Plus':combo.key.toUpperCase())].join('+');
  }

  class Preferences {
    constructor(storage) {
      this.storage = storage;
      this.load();
    }
    load() {
      this.overrides = {};
      this.playFromSkimmer = true;
      try {
        this.playFromSkimmer = this.storage?.getItem(skimKey)!=='false';
        const saved = JSON.parse(this.storage?.getItem(preferenceKey) || '{}');
        for (const {id} of definitions) {
          if (Array.isArray(saved?.[id])) {
            this.overrides[id] = saved[id].filter(combo=>typeof combo?.key==='string')
              .map(combo=>key(combo.key,combo));
          }
        }
      } catch (_) {}
    }
    combos(action) { return this.overrides[action.id] || action.defaults; }
    action(combo) {
      return definitions.find(action=>this.combos(action).some(binding=>equal(combo,binding)))
        || (combo.control && !combo.command && definitions.find(action=>this.combos(action)
          .some(binding=>equal({...combo,control:false,command:true},binding))));
    }
    set(action,combo) {
      let taken;
      for (const other of definitions) {
        const combos = this.combos(other);
        if (other===action || !combos.some(binding=>equal(combo,binding))) continue;
        this.overrides[other.id] = combos.filter(binding=>!equal(binding,combo));
        taken = other;
      }
      this.overrides[action.id] = [combo];
      this.save();
      return taken;
    }
    clear(action) { this.overrides[action.id]=[];this.save(); }
    reset(action) { delete this.overrides[action.id];this.save(); }
    resetAll() { this.overrides={};this.save(); }
    save() {
      try {
        if (Object.keys(this.overrides).length) this.storage?.setItem(preferenceKey,JSON.stringify(this.overrides));
        else this.storage?.removeItem(preferenceKey);
      } catch (_) {}
    }
    setSkim(enabled) {
      this.playFromSkimmer=enabled;
      try { this.storage?.setItem(skimKey,String(enabled)); } catch (_) {}
    }
  }

  return {definitions,Preferences,fromEvent,display,keyboardShortcut,preferenceKey,skimKey};
})();

if (typeof module !== 'undefined') module.exports = StudioShortcuts;
