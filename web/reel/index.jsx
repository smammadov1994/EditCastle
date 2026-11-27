import React from 'react';
import {AbsoluteFill, Composition, Img, OffthreadVideo, Sequence, interpolate, registerRoot, staticFile, useCurrentFrame} from 'remotion';

const canvas = {
  alignItems:'center', justifyContent:'center', overflow:'hidden',
  color:'#f9f5e9', background:'#171610', fontFamily:'Helvetica Neue, sans-serif'
};
const screenshots = {
  width:'91%', height:'auto', borderRadius:17, border:'2px solid #887e69',
  boxShadow:'0 26px 85px #100c06e6'
};

function Editor({closing=false}) {
  const frame=useCurrentFrame();
  const scale=closing ? interpolate(frame,[0,72],[1.052,1.016],{extrapolateRight:'clamp'})
    : interpolate(frame,[0,92],[1.016,1.074],{extrapolateRight:'clamp'});
  return (
    <AbsoluteFill style={{...canvas, background:'radial-gradient(ellipse at 54% 24%, #554f40, #2a251a 59%, #171610)'}}>
      <Img src={staticFile('studio-demo.jpg')} style={{...screenshots,transform:'scale('+scale+')'}}/>
    </AbsoluteFill>
  );
}

function Footage({file,title,inPoint}) {
  const frame=useCurrentFrame();
  const text=interpolate(frame,[0,9],[0,1],{extrapolateRight:'clamp'});
  return (
    <AbsoluteFill style={canvas}>
      <OffthreadVideo src={staticFile(file)} trimBefore={inPoint} muted style={{width:'100%',height:'100%',objectFit:'cover'}}/>
      <AbsoluteFill style={{background:'linear-gradient(transparent 65%, #110e08c0)'}}/>
      <div style={{position:'absolute',left:44,bottom:38,opacity:text,transform:'translateY('+((1-text)*9)+'px)',
        fontSize:20,fontWeight:660,textShadow:'0 2px 14px #171610'}}>{title}</div>
    </AbsoluteFill>
  );
}

function StudioReel() {
  return (
    <AbsoluteFill style={canvas}>
      <Sequence durationInFrames={84}><Editor/></Sequence>
      <Sequence from={84} durationInFrames={63}><Footage file="reel-flying-turtle.mp4" title="Flying Turtle" inPoint={149}/></Sequence>
      <Sequence from={147} durationInFrames={55}><Footage file="reel-kite-dragon.mp4" title="Kite and Dragon" inPoint={188}/></Sequence>
      <Sequence from={202} durationInFrames={55}><Footage file="reel-paper-boat.mp4" title="Paper Boat" inPoint={34}/></Sequence>
      <Sequence from={257}><Editor closing/></Sequence>
    </AbsoluteFill>
  );
}

const Root=()=> <Composition id="StudioReel" component={StudioReel} fps={24} width={1440} height={900} durationInFrames={326}/>;
registerRoot(Root);
