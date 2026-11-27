const gradePreview = (() => {
  const grid = 32;
  const glsl = {
    vertex: `#version 300 es
      out vec2 uv;
      void main() {
        vec2 p = vec2(gl_VertexID == 1 ? 3.0 : -1.0, gl_VertexID == 2 ? 3.0 : -1.0);
        uv = vec2((p.x + 1.0) * .5, (1.0 - p.y) * .5);
        gl_Position = vec4(p, 0., 1.);
      }`,
    fragment: `#version 300 es
      precision highp float;
      precision highp sampler3D;
      in vec2 uv;
      uniform sampler2D picture;
      uniform sampler3D grade;
      out vec4 pixel;
      void main() {
        vec4 frame = texture(picture, uv);
        vec3 coordinate = (clamp(frame.rgb, 0., 1.) * 31. + .5) / 32.;
        pixel = vec4(texture(grade, coordinate).rgb, frame.a);
      }`,
  };
  function createGpu() {
    const canvas = document.createElement('canvas');
    const gl = canvas.getContext('webgl2',{alpha:true,antialias:false,depth:false,stencil:false});
    if (!gl) return null;
    function shader(type,source) {
      const compiled = gl.createShader(type);
      gl.shaderSource(compiled,source);
      gl.compileShader(compiled);
      if (!gl.getShaderParameter(compiled,gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(compiled));
      return compiled;
    }
    const program = gl.createProgram();
    const vertex = shader(gl.VERTEX_SHADER,glsl.vertex);
    const fragment = shader(gl.FRAGMENT_SHADER,glsl.fragment);
    gl.attachShader(program,vertex);
    gl.attachShader(program,fragment);
    gl.linkProgram(program);
    gl.deleteShader(vertex);
    gl.deleteShader(fragment);
    if (!gl.getProgramParameter(program,gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(program));
    gl.useProgram(program);
    gl.uniform1i(gl.getUniformLocation(program,'picture'),0);
    gl.uniform1i(gl.getUniformLocation(program,'grade'),1);
    gl.activeTexture(gl.TEXTURE0);
    const frame = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D,frame);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MIN_FILTER,gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MAG_FILTER,gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_WRAP_S,gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_WRAP_T,gl.CLAMP_TO_EDGE);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT,1);
    // Canvas and FFmpeg both sample the decoded display-referred RGB values.
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL,gl.NONE);
    let frameSize = '';
    return {
      paint(source,entry,width,height) {
        if (gl.isContextLost()) return null;
        if (canvas.width !== width) canvas.width = width;
        if (canvas.height !== height) canvas.height = height;
        gl.viewport(0,0,width,height);
        gl.activeTexture(gl.TEXTURE0);
        const size = [source.videoWidth || source.width,source.videoHeight || source.height].join('/');
        if (size !== frameSize) {
          gl.texImage2D(gl.TEXTURE_2D,0,gl.RGBA,gl.RGBA,gl.UNSIGNED_BYTE,source);
          frameSize = size;
        } else gl.texSubImage2D(gl.TEXTURE_2D,0,0,0,gl.RGBA,gl.UNSIGNED_BYTE,source);
        gl.activeTexture(gl.TEXTURE1);
        if (!entry.texture) {
          entry.texture = gl.createTexture();
          gl.bindTexture(gl.TEXTURE_3D,entry.texture);
          gl.texStorage3D(gl.TEXTURE_3D,1,gl.RGB8,grid,grid,grid);
          gl.texSubImage3D(gl.TEXTURE_3D,0,0,0,0,grid,grid,grid,gl.RGB,gl.UNSIGNED_BYTE,entry.bytes);
          for (const param of [gl.TEXTURE_MIN_FILTER,gl.TEXTURE_MAG_FILTER]) gl.texParameteri(gl.TEXTURE_3D,param,gl.LINEAR);
          for (const param of [gl.TEXTURE_WRAP_S,gl.TEXTURE_WRAP_T,gl.TEXTURE_WRAP_R]) gl.texParameteri(gl.TEXTURE_3D,param,gl.CLAMP_TO_EDGE);
        } else gl.bindTexture(gl.TEXTURE_3D,entry.texture);
        gl.drawArrays(gl.TRIANGLES,0,3);
        return canvas;
      },
      free(entry) {
        if (entry.texture) gl.deleteTexture(entry.texture);
        entry.texture = null;
      },
    };
  }

  const lows = new Uint8Array(256);
  const weights = new Float32Array(256);
  for (let v=0;v<256;v++) {
    const index = v*(grid-1)/255;
    lows[v] = Math.min(grid-2,Math.floor(index));
    weights[v] = index-lows[v];
  }
  function sample(bytes,r,g,b,out,offset) {
    const i=((lows[b]*grid+lows[g])*grid+lows[r])*3;
    const x=weights[r], y=weights[g], z=weights[b];
    const row=grid*3, face=grid*row;
    for (let channel=0;channel<3;channel++) {
      const a=i+channel;
      const low=(bytes[a]*(1-x)+bytes[a+3]*x)*(1-y)+(bytes[a+row]*(1-x)+bytes[a+row+3]*x)*y;
      const h=a+face;
      const high=(bytes[h]*(1-x)+bytes[h+3]*x)*(1-y)+(bytes[h+row]*(1-x)+bytes[h+row+3]*x)*y;
      out[offset+channel]=Math.round(low+(high-low)*z);
    }
  }
  let gpu;
  let cpu;
  function paint(source,entry,width,height) {
    if (!entry) return source;
    if (gpu === undefined) {
      try {gpu=createGpu();} catch (_) {gpu=null;}
    }
    const scaled = Math.min(1,(source.videoWidth || source.width)/width,(source.videoHeight || source.height)/height);
    width=Math.max(1,Math.round(width*scaled));
    height=Math.max(1,Math.round(height*scaled));
    const output = gpu?.paint(source,entry,width,height);
    if (output) return output;
    cpu ||= document.createElement('canvas');
    const fallback = Math.min(1,720/Math.max(width,height));
    const w=Math.max(1,Math.round(width*fallback)), h=Math.max(1,Math.round(height*fallback));
    if (cpu.width!==w) cpu.width=w;
    if (cpu.height!==h) cpu.height=h;
    const context=cpu.getContext('2d',{willReadFrequently:true});
    context.drawImage(source,0,0,w,h);
    const image=context.getImageData(0,0,w,h);
    const data=image.data;
    for (let i=0;i<data.length;i+=4) sample(entry.bytes,data[i],data[i+1],data[i+2],data,i);
    context.putImageData(image,0,0);
    return cpu;
  }
  function swatch(input,entry) {
    if (!entry) return input;
    const match=String(input || '').match(/^#([a-f0-9]{6})$/i);
    if (!match) return input;
    const value=parseInt(match[1],16);
    const result=[];
    sample(entry.bytes,(value >> 16)&255,(value >> 8)&255,value&255,result,0);
    return 'rgb('+result.join(',')+')';
  }
  return {
    paint,swatch,grid,
    free(entry) {gpu?.free(entry);},
  };
})();
