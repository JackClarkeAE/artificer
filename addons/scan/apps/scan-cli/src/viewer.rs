//! Self-contained HTML viewer: original scan on the left, classified
//! segmentation on the right, orbiting in lockstep. Geometry is decimated
//! for display only; all reported numbers come from the full-resolution
//! pipeline.

use artificer_scan_core::TriangleMesh;
use artificer_scan_core::report::ReverseReport;
use artificer_scan_core::segment::SurfaceClass;

const DISPLAY_TRIANGLE_BUDGET: usize = 160_000;

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}

const FREEFORM_COLOR: [u8; 3] = [186, 189, 194];

/// One visually distinct colour per classified feature: golden-angle hue
/// stepping never puts similar hues next to each other, and cycling
/// lightness separates features that land on nearby hues anyway.
fn feature_color(instance: usize) -> [u8; 3] {
    let hue = (instance as f64 * 137.508).rem_euclid(360.0);
    let lightness = [0.44, 0.58, 0.68][instance % 3];
    hsl_to_rgb(hue, 0.70, lightness)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn build_viewer_html(mesh: &TriangleMesh, report: &ReverseReport, title: &str) -> String {
    // Show the part in its datum frame when one was detected, so what the
    // user orbits matches the coordinates in the report.
    let aligned;
    let mesh = match &report.datum {
        Some(alignment) => {
            aligned = mesh.transformed(&alignment.transform);
            &aligned
        }
        None => mesh,
    };
    // Decimate for display when the scan is heavy.
    let (display, origins) = if mesh.triangles().len() > DISPLAY_TRIANGLE_BUDGET {
        let mut cell = mesh.bounds_diagonal() / 260.0;
        let mut result = mesh.simplified_by_clustering(cell);
        while result.0.triangles().len() > DISPLAY_TRIANGLE_BUDGET {
            cell *= 1.35;
            result = mesh.simplified_by_clustering(cell);
        }
        result
    } else {
        let identity: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
        (mesh.clone(), identity)
    };
    // Color per original face from the classification.
    let mut face_colors = vec![FREEFORM_COLOR; mesh.triangles().len()];
    let mut classified_count = 0usize;
    let mut freeform_count = 0usize;
    let mut legend: Vec<(String, [u8; 3], f64)> = Vec::new();
    for feature in &report.features {
        let color = if matches!(feature.surface, SurfaceClass::Freeform) {
            freeform_count += 1;
            if feature.face_count >= 50 {
                // Large unclassified patches get subtly varied neutral
                // tints so the segmentation structure stays visible.
                hsl_to_rgb(
                    200.0 + ((freeform_count * 53) % 7) as f64 * 8.0,
                    0.10,
                    0.48 + ((freeform_count * 37) % 5) as f64 * 0.07,
                )
            } else {
                FREEFORM_COLOR
            }
        } else {
            classified_count += 1;
            feature_color(classified_count)
        };
        for &face in &feature.faces {
            face_colors[face as usize] = color;
        }
        if !matches!(feature.surface, SurfaceClass::Freeform) {
            let label = match &feature.surface {
                SurfaceClass::Plane(fit) => format!(
                    "plane n({:+.2} {:+.2} {:+.2})",
                    fit.normal.x, fit.normal.y, fit.normal.z
                ),
                SurfaceClass::Cylinder(fit) => {
                    format!("cylinder d {:.2} mm", fit.radius * 2.0)
                }
                SurfaceClass::Sphere(fit) => format!("sphere d {:.2} mm", fit.radius * 2.0),
                SurfaceClass::Cone(fit) => {
                    format!("cone {:.1} deg", fit.half_angle.to_degrees())
                }
                SurfaceClass::Blend(fit) => {
                    format!("fillet r {:.2} mm", fit.minor_radius)
                }
                SurfaceClass::Freeform => unreachable!(),
            };
            legend.push((label, color, feature.area));
        }
    }
    legend.sort_by(|a, b| b.2.total_cmp(&a.2));
    legend.truncate(14);
    // De-index for flat shading: 9 floats and 9 color bytes per triangle.
    let mut positions = Vec::with_capacity(display.triangles().len() * 36);
    let mut colors = Vec::with_capacity(display.triangles().len() * 9);
    for (face, _) in display.triangles().iter().enumerate() {
        let color = face_colors[origins[face] as usize];
        for corner in display.triangle_points(face) {
            for value in [corner.x, corner.y, corner.z] {
                positions.extend_from_slice(&(value as f32).to_le_bytes());
            }
            colors.extend_from_slice(&color);
        }
    }
    let bounds = mesh.bounds();
    let (center, diagonal) = bounds.map_or(([0.0f64; 3], 1.0), |b| {
        (
            [
                (b.min.x + b.max.x) / 2.0,
                (b.min.y + b.max.y) / 2.0,
                (b.min.z + b.max.z) / 2.0,
            ],
            (b.max - b.min).length(),
        )
    });
    let classified_percent = if report.total_area > 0.0 {
        100.0 * report.classified_area / report.total_area
    } else {
        0.0
    };
    let legend_json: String = {
        let mut out = String::from("[");
        for (index, (label, color, area)) in legend.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"label\":\"{}\",\"color\":[{},{},{}],\"area\":{:.1}}}",
                label.replace('"', ""),
                color[0],
                color[1],
                color[2],
                area
            ));
        }
        out.push(']');
        out
    };
    let stats = format!(
        "{} vertices | {} triangles | {:.0} mm&sup2; | {} regions | {:.1}% classified | display {} tris",
        mesh.positions().len(),
        mesh.triangles().len(),
        report.total_area,
        report.features.len(),
        classified_percent,
        display.triangles().len()
    );
    TEMPLATE
        .replace("__TITLE__", &escape_html(title))
        .replace("__STATS__", &stats)
        .replace("__CENTER__", &format!("[{},{},{}]", center[0], center[1], center[2]))
        .replace("__DIAG__", &format!("{diagonal}"))
        .replace("__LEGEND__", &legend_json)
        .replace("__POS__", &base64(&positions))
        .replace("__COL__", &base64(&colors))
}

const TEMPLATE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>__TITLE__ - Artificer Scan to CAD</title>
<style>
  html,body{margin:0;height:100%;background:#141619;color:#d7dade;font:13px/1.4 system-ui,sans-serif;overflow:hidden}
  #bar{position:fixed;top:0;left:0;right:0;padding:8px 14px;background:#1c1f24;border-bottom:1px solid #2a2e35;z-index:2;display:flex;gap:18px;align-items:baseline}
  #bar b{color:#fff;font-size:14px}
  #bar span{color:#9aa1ab}
  #panes{display:flex;height:100%;padding-top:38px;box-sizing:border-box}
  .pane{flex:1;position:relative;min-width:0}
  .pane+.pane{border-left:1px solid #2a2e35}
  canvas{width:100%;height:100%;display:block;touch-action:none;cursor:grab}
  .tag{position:absolute;top:10px;left:12px;background:#0009;padding:4px 10px;border-radius:4px;font-weight:600;pointer-events:none}
  #legend{position:absolute;bottom:12px;right:12px;background:#0009;padding:8px 12px;border-radius:6px;max-height:45%;overflow:auto}
  #legend div{display:flex;align-items:center;gap:8px;margin:2px 0;white-space:nowrap}
  #legend i{width:12px;height:12px;border-radius:3px;display:inline-block;flex:none}
  #hint{position:fixed;bottom:10px;left:14px;color:#7c828c;z-index:2}
</style></head><body>
<div id="bar"><b>__TITLE__</b><span>__STATS__</span></div>
<div id="panes">
  <div class="pane"><canvas id="left"></canvas><div class="tag">Original scan</div></div>
  <div class="pane"><canvas id="right"></canvas><div class="tag">Artificer Scan-to-CAD segmentation</div><div id="legend"></div></div>
</div>
<div id="hint">drag: orbit &nbsp; wheel: zoom &nbsp; shift-drag: pan</div>
<script>
"use strict";
const CENTER=__CENTER__, DIAG=__DIAG__, LEGEND=__LEGEND__;
function decode(b64){const s=atob(b64);const a=new Uint8Array(s.length);for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a;}
const posBytes=decode("__POS__");
const colBytes=decode("__COL__");
const positions=new Float32Array(posBytes.buffer);
const vertexCount=positions.length/3;

const legendBox=document.getElementById("legend");
for(const item of LEGEND){
  const row=document.createElement("div");
  const chip=document.createElement("i");chip.style.background=`rgb(${item.color.join(",")})`;
  row.appendChild(chip);
  row.appendChild(document.createTextNode(`${item.label} (${item.area.toFixed(0)} mm²)`));
  legendBox.appendChild(row);
}
if(!LEGEND.length){legendBox.textContent="no analytic features passed tolerance yet";}

const cam={theta:0.9,phi:1.05,radius:DIAG*1.5,target:CENTER.slice(),pan:[0,0,0]};

function perspective(fov,aspect,near,far){const f=1/Math.tan(fov/2),m=new Float32Array(16);
 m[0]=f/aspect;m[5]=f;m[10]=(far+near)/(near-far);m[11]=-1;m[14]=2*far*near/(near-far);return m;}
function lookAt(eye,target,up){
 let z=[eye[0]-target[0],eye[1]-target[1],eye[2]-target[2]];let zl=Math.hypot(...z);z=z.map(v=>v/zl);
 let x=[up[1]*z[2]-up[2]*z[1],up[2]*z[0]-up[0]*z[2],up[0]*z[1]-up[1]*z[0]];let xl=Math.hypot(...x);x=x.map(v=>v/xl);
 const y=[z[1]*x[2]-z[2]*x[1],z[2]*x[0]-z[0]*x[2],z[0]*x[1]-z[1]*x[0]];
 const m=new Float32Array(16);
 m[0]=x[0];m[4]=x[1];m[8]=x[2];m[1]=y[0];m[5]=y[1];m[9]=y[2];m[2]=z[0];m[6]=z[1];m[10]=z[2];
 m[12]=-(x[0]*eye[0]+x[1]*eye[1]+x[2]*eye[2]);
 m[13]=-(y[0]*eye[0]+y[1]*eye[1]+y[2]*eye[2]);
 m[14]=-(z[0]*eye[0]+z[1]*eye[1]+z[2]*eye[2]);m[15]=1;return m;}
function mul(a,b){const m=new Float32Array(16);
 for(let c=0;c<4;c++)for(let r=0;r<4;r++){let s=0;for(let k=0;k<4;k++)s+=a[k*4+r]*b[c*4+k];m[c*4+r]=s;}return m;}

const VS=`#version 300 es
in vec3 pos;in vec3 col;uniform mat4 mvp;uniform mat4 mv;out vec3 vcol;out vec3 vpos;
void main(){gl_Position=mvp*vec4(pos,1.0);vpos=(mv*vec4(pos,1.0)).xyz;vcol=col;}`;
const FS=`#version 300 es
precision mediump float;in vec3 vcol;in vec3 vpos;out vec4 frag;
void main(){vec3 n=normalize(cross(dFdx(vpos),dFdy(vpos)));
 float light=0.30+0.62*abs(n.z)+0.08*abs(n.y);
 frag=vec4(vcol*light,1.0);}`;

function makePane(canvasId,useColors){
  const canvas=document.getElementById(canvasId);
  const gl=canvas.getContext("webgl2",{antialias:true});
  if(!gl){canvas.replaceWith("WebGL2 unavailable");return null;}
  const prog=gl.createProgram();
  for(const [type,src] of [[gl.VERTEX_SHADER,VS],[gl.FRAGMENT_SHADER,FS]]){
    const sh=gl.createShader(type);gl.shaderSource(sh,src);gl.compileShader(sh);
    if(!gl.getShaderParameter(sh,gl.COMPILE_STATUS))console.error(gl.getShaderInfoLog(sh));
    gl.attachShader(prog,sh);}
  gl.linkProgram(prog);gl.useProgram(prog);
  const posBuf=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,posBuf);
  gl.bufferData(gl.ARRAY_BUFFER,positions,gl.STATIC_DRAW);
  const posLoc=gl.getAttribLocation(prog,"pos");
  gl.enableVertexAttribArray(posLoc);gl.vertexAttribPointer(posLoc,3,gl.FLOAT,false,0,0);
  const colLoc=gl.getAttribLocation(prog,"col");
  if(useColors){
    const colBuf=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,colBuf);
    gl.bufferData(gl.ARRAY_BUFFER,colBytes,gl.STATIC_DRAW);
    gl.enableVertexAttribArray(colLoc);gl.vertexAttribPointer(colLoc,3,gl.UNSIGNED_BYTE,true,0,0);
  }else{
    gl.disableVertexAttribArray(colLoc);gl.vertexAttrib3f(colLoc,0.72,0.73,0.75);
  }
  gl.enable(gl.DEPTH_TEST);gl.clearColor(0.078,0.086,0.098,1);
  return {canvas,gl,prog};
}

const panes=[makePane("left",false),makePane("right",true)].filter(Boolean);

function render(){
  const st=Math.sin(cam.theta),sp=Math.sin(cam.phi),cp=Math.cos(cam.phi);
  const t=[cam.target[0]+cam.pan[0],cam.target[1]+cam.pan[1],cam.target[2]+cam.pan[2]];
  const eye=[t[0]+cam.radius*sp*Math.cos(cam.theta),t[1]+cam.radius*sp*st,t[2]+cam.radius*cp];
  for(const pane of panes){
    const dpr=window.devicePixelRatio||1;
    const w=pane.canvas.clientWidth*dpr,h=pane.canvas.clientHeight*dpr;
    if(pane.canvas.width!==w||pane.canvas.height!==h){pane.canvas.width=w;pane.canvas.height=h;}
    const gl=pane.gl;
    gl.viewport(0,0,w,h);
    gl.clear(gl.COLOR_BUFFER_BIT|gl.DEPTH_BUFFER_BIT);
    const mv=lookAt(eye,t,[0,0,1]);
    const proj=perspective(0.8,w/h,DIAG*0.01,DIAG*20);
    gl.uniformMatrix4fv(gl.getUniformLocation(pane.prog,"mv"),false,mv);
    gl.uniformMatrix4fv(gl.getUniformLocation(pane.prog,"mvp"),false,mul(proj,mv));
    gl.drawArrays(gl.TRIANGLES,0,vertexCount);
  }
  requestAnimationFrame(render);
}
requestAnimationFrame(render);

let dragging=false,panning=false,lastX=0,lastY=0;
for(const pane of panes){
  const el=pane.canvas;
  el.addEventListener("pointerdown",e=>{dragging=true;panning=e.shiftKey||e.button===2;lastX=e.clientX;lastY=e.clientY;el.setPointerCapture(e.pointerId);});
  el.addEventListener("pointerup",()=>{dragging=false;});
  el.addEventListener("pointermove",e=>{
    if(!dragging)return;
    const dx=e.clientX-lastX,dy=e.clientY-lastY;lastX=e.clientX;lastY=e.clientY;
    if(panning){
      const scale=cam.radius*0.0016;
      const st=Math.sin(cam.theta),ct=Math.cos(cam.theta);
      cam.pan[0]+=dx*scale*st;
      cam.pan[1]-=dx*scale*ct;
      cam.pan[2]+=dy*scale;
    }else{
      cam.theta-=dx*0.008;cam.phi=Math.min(3.05,Math.max(0.08,cam.phi-dy*0.008));
    }
  });
  el.addEventListener("wheel",e=>{e.preventDefault();cam.radius*=Math.exp(e.deltaY*0.0012);cam.radius=Math.min(DIAG*10,Math.max(DIAG*0.15,cam.radius));},{passive:false});
  el.addEventListener("contextmenu",e=>e.preventDefault());
}
</script></body></html>
"#;
