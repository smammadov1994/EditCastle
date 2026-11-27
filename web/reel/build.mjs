import {spawnSync} from 'node:child_process';
import {copyFileSync, mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';

const output = new URL('../static/', import.meta.url);
const footage = new URL('../static/landing-media/', import.meta.url);
const publicDir = mkdtempSync(join(tmpdir(), 'editcastle-reel-'));
const cli = new URL('./node_modules/@remotion/cli/remotion-cli.js', import.meta.url);
const common = ['index.jsx', 'StudioReel'];
try {
  for (const file of ['reel-flying-turtle.mp4','reel-kite-dragon.mp4','reel-paper-boat.mp4']) {
    copyFileSync(new URL(file, footage), join(publicDir,file));
  }
  copyFileSync(new URL('studio-demo.jpg', output),join(publicDir,'studio-demo.jpg'));
  const renders = [
    ['render', '../static/studio-reel.mp4', '--codec=h264', '--concurrency=4', '--crf=22','--muted'],
    ['still', '../static/studio-reel.jpg', '--frame=55', '--image-format=jpeg', '--jpeg-quality=88']
  ];
  for (const [command, target, ...options] of renders) {
    const result=spawnSync(process.execPath, [
      decodeURIComponent(cli.pathname),command,...common,target,'--public-dir',publicDir,...options,...process.argv.slice(2)
    ],{cwd:new URL('.',import.meta.url),stdio:'inherit'});
    if (result.error) throw result.error;
    if (result.status!==0) throw new Error('Remotion '+command+' failed.');
  }
} finally {
  rmSync(publicDir,{recursive:true,force:true});
}
