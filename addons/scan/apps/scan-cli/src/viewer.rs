//! Self-contained HTML inspection viewer: original scan on the left, the
//! classified segmentation or the sharp rebuild on the right, orbiting in
//! lockstep. Every feature is a clickable coloured section — clicking
//! reports its feature id for review. Geometry is decimated for display
//! only; all reported numbers come from the full-resolution pipeline.

use artificer_scan_core::TriangleMesh;
use artificer_scan_core::rebuild::RebuiltModel;
use artificer_scan_core::report::ReverseReport;
use artificer_scan_core::segment::SurfaceClass;

const DISPLAY_TRIANGLE_BUDGET: usize = 160_000;

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
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

/// One visually distinct colour per feature id: golden-angle hue stepping
/// never puts similar hues next to each other, and cycling lightness
/// separates features that land on nearby hues anyway.
pub(crate) fn feature_color(instance: usize) -> [u8; 3] {
    let hue = (instance as f64 * 137.508).rem_euclid(360.0);
    let lightness = [0.44, 0.58, 0.68][instance % 3];
    hsl_to_rgb(hue, 0.70, lightness)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn feature_label(surface: &SurfaceClass) -> String {
    match surface {
        SurfaceClass::Plane(fit) => format!(
            "plane n({:+.2} {:+.2} {:+.2}) z {:+.2}",
            fit.normal.x, fit.normal.y, fit.normal.z, fit.origin.z
        ),
        SurfaceClass::Cylinder(fit) => format!("cylinder d {:.2} mm", fit.radius * 2.0),
        SurfaceClass::Sphere(fit) => format!("sphere d {:.2} mm", fit.radius * 2.0),
        SurfaceClass::Cone(fit) => format!("cone {:.1} deg", fit.half_angle.to_degrees()),
        SurfaceClass::Blend(fit) => format!("fillet r {:.2} mm", fit.minor_radius),
        SurfaceClass::Torus(fit) => format!(
            "torus R {:.1} r {:.1} mm",
            fit.major_radius, fit.minor_radius
        ),
        SurfaceClass::Pattern(fit) => format!("pattern x {} (toothing)", fit.count),
        SurfaceClass::EdgeRound(fit) => format!("edge round, span {:.1} mm", fit.span),
        SurfaceClass::Freeform => "freeform".to_owned(),
    }
}

/// Colour and feature id per original mesh face, keyed by feature id so
/// a reported id is unambiguous.
fn face_paint(face_count: usize, report: &ReverseReport) -> (Vec<[u8; 3]>, Vec<u32>) {
    let mut colors = vec![FREEFORM_COLOR; face_count];
    let mut ids = vec![u32::MAX; face_count];
    for feature in &report.features {
        let color = if matches!(feature.surface, SurfaceClass::Freeform) {
            FREEFORM_COLOR
        } else {
            feature_color(feature.id + 1)
        };
        for &face in &feature.faces {
            colors[face as usize] = color;
            ids[face as usize] = feature.id as u32;
        }
    }
    (colors, ids)
}

/// Legend row: label, colour, and area of one classified feature.
pub type LegendEntry = (String, [u8; 3], f64);

/// Display-ready geometry shared by the HTML viewer and the offline
/// snapshot renderer: datum-aligned, decimated, one colour per face.
pub struct DisplayModel {
    pub mesh: TriangleMesh,
    /// Colour per display triangle.
    pub colors: Vec<[u8; 3]>,
    /// Feature id per display triangle.
    pub feature_ids: Vec<u32>,
    pub legend: Vec<LegendEntry>,
}

pub fn display_model(mesh: &TriangleMesh, report: &ReverseReport) -> DisplayModel {
    // Show the part in its datum frame when one was detected, so what the
    // user sees matches the coordinates in the report.
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
    let (face_colors, face_ids) = face_paint(mesh.triangles().len(), report);
    let colors: Vec<[u8; 3]> = origins
        .iter()
        .map(|&origin| face_colors[origin as usize])
        .collect();
    let feature_ids: Vec<u32> = origins
        .iter()
        .map(|&origin| face_ids[origin as usize])
        .collect();
    let mut legend: Vec<LegendEntry> = report
        .features
        .iter()
        .filter(|f| !matches!(f.surface, SurfaceClass::Freeform))
        .map(|f| {
            (
                format!("#{} {}", f.id, feature_label(&f.surface)),
                feature_color(f.id + 1),
                f.area,
            )
        })
        .collect();
    legend.sort_by(|a, b| b.2.total_cmp(&a.2));
    legend.truncate(14);
    DisplayModel {
        mesh: display,
        colors,
        feature_ids,
        legend,
    }
}

/// De-indexes a mesh into flat position/colour/id buffers for the viewer.
fn pack_buffers(
    mesh: &TriangleMesh,
    colors: &[[u8; 3]],
    ids: &[u32],
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut positions = Vec::with_capacity(mesh.triangles().len() * 36);
    let mut color_bytes = Vec::with_capacity(mesh.triangles().len() * 9);
    let mut id_bytes = Vec::with_capacity(mesh.triangles().len() * 12);
    for face in 0..mesh.triangles().len() {
        let color = colors[face];
        let id = if ids[face] == u32::MAX {
            -1.0
        } else {
            ids[face] as f32
        };
        for corner in mesh.triangle_points(face) {
            for value in [corner.x, corner.y, corner.z] {
                positions.extend_from_slice(&(value as f32).to_le_bytes());
            }
            color_bytes.extend_from_slice(&color);
            id_bytes.extend_from_slice(&id.to_le_bytes());
        }
    }
    (positions, color_bytes, id_bytes)
}

/// Deviation bands the map distinguishes before giving up and calling a
/// face unmatched.
const DEVIATION_BANDS: u8 = 4;

/// The heat ramp: green inside tolerance, through yellow and orange, to
/// red at the edge of what is still a match, and blue-grey for geometry
/// with no counterpart at all.
///
/// Unmatched gets its own colour rather than "very red", because it is a
/// different statement: not "this is off by a lot" but "there is nothing
/// here to be off from", and a reader who cannot tell those apart cannot
/// act on either.
fn deviation_color(band: Option<u8>) -> [u8; 3] {
    match band {
        Some(0) => [64, 190, 120],
        Some(1) => [190, 210, 90],
        Some(2) => [235, 180, 70],
        Some(3) => [230, 120, 60],
        Some(_) => [215, 70, 70],
        None => [90, 110, 150],
    }
}

pub fn build_viewer_html(
    mesh: &TriangleMesh,
    report: &ReverseReport,
    rebuilt: Option<&RebuiltModel>,
    title: &str,
) -> String {
    let model = display_model(mesh, report);
    let (seg_pos, seg_col, seg_fid) = pack_buffers(&model.mesh, &model.colors, &model.feature_ids);
    let (reb_pos, reb_col, reb_fid, reb_dev) = match rebuilt {
        Some(rebuilt) => {
            let colors: Vec<[u8; 3]> = rebuilt
                .feature_of_face
                .iter()
                .map(|&id| feature_color(id + 1))
                .collect();
            let ids: Vec<u32> = rebuilt
                .feature_of_face
                .iter()
                .map(|&id| id as u32)
                .collect();
            let (pos, col, fid) = pack_buffers(&rebuilt.mesh, &colors, &ids);
            // The deviation map: how far each emitted face sits from the
            // scan it claims to represent. Everything needed for this has
            // been computed for a while; only the colours were missing.
            let deviation = match &report.datum {
                Some(alignment) => {
                    let bands = artificer_scan_core::coverage::deviation_bands(
                        &rebuilt.mesh,
                        mesh,
                        alignment,
                        report.tolerance,
                        DEVIATION_BANDS,
                        false,
                    );
                    let painted: Vec<[u8; 3]> =
                        bands.iter().map(|band| deviation_color(*band)).collect();
                    pack_buffers(&rebuilt.mesh, &painted, &ids).1
                }
                None => col.clone(),
            };
            (pos, col, fid, deviation)
        }
        None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
    };
    let bounds = model.mesh.bounds();
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
    // Feature metadata for the selection panel, indexed by id.
    let meta_json: String = {
        let mut out = String::from("[");
        for (index, feature) in report.features.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let color = if matches!(feature.surface, SurfaceClass::Freeform) {
                FREEFORM_COLOR
            } else {
                feature_color(feature.id + 1)
            };
            out.push_str(&format!(
                "{{\"id\":{},\"kind\":\"{}\",\"label\":\"{}\",\"area\":{:.1},\"color\":[{},{},{}]}}",
                feature.id,
                feature.surface.kind(),
                feature_label(&feature.surface).replace('"', ""),
                feature.area,
                color[0],
                color[1],
                color[2]
            ));
        }
        out.push(']');
        out
    };
    let legend_json: String = {
        let mut out = String::from("[");
        for (index, (label, color, area)) in model.legend.iter().enumerate() {
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
        "{} vertices | {} triangles | {:.0} mm&sup2; | {} features | {:.1}% classified | click a section to select it",
        mesh.positions().len(),
        mesh.triangles().len(),
        report.total_area,
        report.features.len(),
        classified_percent
    );
    TEMPLATE
        .replace("__TITLE__", &escape_html(title))
        .replace("__STATS__", &stats)
        .replace(
            "__CENTER__",
            &format!("[{},{},{}]", center[0], center[1], center[2]),
        )
        .replace("__DIAG__", &format!("{diagonal}"))
        .replace("__META__", &meta_json)
        .replace("__LEGEND__", &legend_json)
        .replace("__SEGPOS__", &base64(&seg_pos))
        .replace("__SEGCOL__", &base64(&seg_col))
        .replace("__SEGFID__", &base64(&seg_fid))
        .replace("__REBPOS__", &base64(&reb_pos))
        .replace("__REBCOL__", &base64(&reb_col))
        .replace("__REBFID__", &base64(&reb_fid))
        .replace("__REBDEV__", &base64(&reb_dev))
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
  #mode{position:absolute;top:8px;right:12px;background:#23262d;border:1px solid #3a3f48;color:#d7dade;padding:5px 12px;border-radius:5px;cursor:pointer;font:inherit}
  #mode:hover{background:#2c3038}
  #devlegend{display:none;position:absolute;top:44px;right:12px;gap:10px;width:max-content;white-space:nowrap;background:#23262db8;border:1px solid #3a3f48;padding:5px 10px;border-radius:5px;font-size:11px;color:#c7cad0;z-index:3}
  #devlegend span{display:flex;align-items:center;gap:4px}
  #devlegend i{width:11px;height:11px;border-radius:2px;display:inline-block}
  #legend{position:absolute;bottom:12px;right:12px;background:#0009;padding:8px 12px;border-radius:6px;max-height:38%;overflow:auto}
  #legend div{display:flex;align-items:center;gap:8px;margin:2px 0;white-space:nowrap}
  #legend i,#selection i{width:12px;height:12px;border-radius:3px;display:inline-block;flex:none}
  #selection{position:absolute;bottom:12px;left:12px;background:#0d0f12ee;border:1px solid #2a2e35;padding:10px 12px;border-radius:6px;max-height:44%;overflow:auto;min-width:260px}
  #selection h4{margin:0 0 6px;color:#fff;font-size:12px;display:flex;justify-content:space-between;gap:10px}
  #selection h4 button{background:#23262d;border:1px solid #3a3f48;color:#d7dade;border-radius:4px;cursor:pointer;font:inherit;padding:1px 8px}
  #selection div.row{display:flex;align-items:center;gap:8px;margin:2px 0;white-space:nowrap;cursor:pointer}
  #selection div.row:hover{color:#fff}
  #ids{margin-top:6px;padding:5px 8px;background:#1c1f24;border-radius:4px;color:#8fd18f;user-select:all;font-family:ui-monospace,monospace}
  #hint{position:fixed;bottom:10px;left:50%;transform:translateX(-50%);color:#7c828c;z-index:2}
</style></head><body>
<div id="bar"><b>__TITLE__</b><span>__STATS__</span></div>
<div id="panes">
  <div class="pane"><canvas id="left"></canvas><div class="tag">Original scan</div></div>
  <div class="pane"><canvas id="right"></canvas><div class="tag" id="rightTag">Segmentation</div>
    <button id="mode">show rebuild</button>
    <div id="devlegend"><span><i style="background:rgb(64,190,120)"></i>&le;1&times;tol</span><span><i style="background:rgb(190,210,90)"></i>2&times;</span><span><i style="background:rgb(235,180,70)"></i>3&times;</span><span><i style="background:rgb(230,120,60)"></i>4&times;</span><span><i style="background:rgb(215,70,70)"></i>&gt;4&times;</span><span><i style="background:rgb(90,110,150)"></i>no counterpart</span></div>
    <div id="legend"></div>
    <div id="selection"><h4>selected features <button id="clear">clear</button></h4>
      <div id="selRows"></div><div id="ids">none — click a coloured section</div></div>
  </div>
</div>
<div id="hint">drag: orbit &nbsp; wheel: zoom &nbsp; shift-drag: pan &nbsp; click: select feature</div>
<script>
"use strict";
const CENTER=__CENTER__, DIAG=__DIAG__, META=__META__, LEGEND=__LEGEND__;
function decode(b64){const s=atob(b64);const a=new Uint8Array(s.length);for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a;}
const seg={pos:new Float32Array(decode("__SEGPOS__").buffer),col:decode("__SEGCOL__"),fid:new Float32Array(decode("__SEGFID__").buffer)};
const rebRaw={pos:decode("__REBPOS__"),col:decode("__REBCOL__"),fid:decode("__REBFID__"),dev:decode("__REBDEV__")};
const reb=rebRaw.pos.length?{pos:new Float32Array(rebRaw.pos.buffer),col:rebRaw.col,fid:new Float32Array(rebRaw.fid.buffer)}:null;
// The rebuild painted by deviation instead of by feature: same geometry,
// same picking ids, different question asked of it.
const dev=reb&&rebRaw.dev.length?{pos:reb.pos,col:rebRaw.dev,fid:reb.fid}:null;

const legendBox=document.getElementById("legend");
for(const item of LEGEND){
  const row=document.createElement("div");
  const chip=document.createElement("i");chip.style.background=`rgb(${item.color.join(",")})`;
  row.appendChild(chip);
  row.appendChild(document.createTextNode(`${item.label} (${item.area.toFixed(0)} mm²)`));
  legendBox.appendChild(row);
}

const cam={theta:0.6,phi:0.7,radius:DIAG*1.1,target:CENTER.slice(),pan:[0,0,0]};
const selected=new Set();

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
in vec3 pos;in vec3 col;in float fid;
uniform mat4 mvp;uniform mat4 mv;
out vec3 vcol;out vec3 vpos;out float vfid;
void main(){gl_Position=mvp*vec4(pos,1.0);vpos=(mv*vec4(pos,1.0)).xyz;vcol=col;vfid=fid;}`;
const FS=`#version 300 es
precision highp float;in vec3 vcol;in vec3 vpos;in float vfid;out vec4 frag;
uniform int pickMode;uniform float sel[16];uniform int selCount;
void main(){
 if(pickMode==1){int i=int(vfid+2.5);frag=vec4(float(i&255)/255.0,float((i>>8)&255)/255.0,float((i>>16)&255)/255.0,1.0);return;}
 vec3 n=normalize(cross(dFdx(vpos),dFdy(vpos)));
 float light=0.30+0.62*abs(n.z)+0.08*abs(n.y);
 vec3 c=vcol*light;
 bool isSel=false;
 for(int k=0;k<16;k++){if(k>=selCount)break;if(abs(vfid-sel[k])<0.5)isSel=true;}
 if(selCount>0){ if(isSel){c=mix(c,vec3(1.0),0.40);} else {c*=0.35;} }
 frag=vec4(c,1.0);}`;

function makePane(canvasId,geometryList){
  const canvas=document.getElementById(canvasId);
  const gl=canvas.getContext("webgl2",{antialias:true,preserveDrawingBuffer:true});
  if(!gl){canvas.replaceWith("WebGL2 unavailable");return null;}
  const prog=gl.createProgram();
  for(const [type,src] of [[gl.VERTEX_SHADER,VS],[gl.FRAGMENT_SHADER,FS]]){
    const sh=gl.createShader(type);gl.shaderSource(sh,src);gl.compileShader(sh);
    if(!gl.getShaderParameter(sh,gl.COMPILE_STATUS))console.error(gl.getShaderInfoLog(sh));
    gl.attachShader(prog,sh);}
  gl.linkProgram(prog);gl.useProgram(prog);
  const sets=[];
  for(const g of geometryList){
    if(!g){sets.push(null);continue;}
    const posBuf=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,posBuf);
    gl.bufferData(gl.ARRAY_BUFFER,g.pos,gl.STATIC_DRAW);
    const colBuf=g.col?gl.createBuffer():null;
    if(colBuf){gl.bindBuffer(gl.ARRAY_BUFFER,colBuf);gl.bufferData(gl.ARRAY_BUFFER,g.col,gl.STATIC_DRAW);}
    const fidBuf=g.fid?gl.createBuffer():null;
    if(fidBuf){gl.bindBuffer(gl.ARRAY_BUFFER,fidBuf);gl.bufferData(gl.ARRAY_BUFFER,g.fid,gl.STATIC_DRAW);}
    sets.push({posBuf,colBuf,fidBuf,count:g.pos.length/3});
  }
  gl.enable(gl.DEPTH_TEST);gl.clearColor(0.078,0.086,0.098,1);
  return {canvas,gl,prog,sets,active:0};
}

function drawPane(pane,pickMode){
  const gl=pane.gl,prog=pane.prog;
  const dpr=window.devicePixelRatio||1;
  const w=pane.canvas.clientWidth*dpr,h=pane.canvas.clientHeight*dpr;
  if(pane.canvas.width!==w||pane.canvas.height!==h){pane.canvas.width=w;pane.canvas.height=h;}
  gl.viewport(0,0,w,h);
  if(pickMode===1){gl.clearColor(0,0,0,1);}else{gl.clearColor(0.078,0.086,0.098,1);}
  gl.clear(gl.COLOR_BUFFER_BIT|gl.DEPTH_BUFFER_BIT);
  const set=pane.sets[pane.active];
  if(!set)return;
  const st=Math.sin(cam.theta),sp=Math.sin(cam.phi),cp=Math.cos(cam.phi);
  const t=[cam.target[0]+cam.pan[0],cam.target[1]+cam.pan[1],cam.target[2]+cam.pan[2]];
  const eye=[t[0]+cam.radius*sp*Math.cos(cam.theta),t[1]+cam.radius*sp*st,t[2]+cam.radius*cp];
  const mv=lookAt(eye,t,[0,0,1]);
  const proj=perspective(0.8,w/h,DIAG*0.01,DIAG*20);
  gl.uniformMatrix4fv(gl.getUniformLocation(prog,"mv"),false,mv);
  gl.uniformMatrix4fv(gl.getUniformLocation(prog,"mvp"),false,mul(proj,mv));
  gl.uniform1i(gl.getUniformLocation(prog,"pickMode"),pickMode);
  const selArr=new Float32Array(16);let i=0;
  for(const id of selected){if(i<16)selArr[i++]=id;}
  gl.uniform1fv(gl.getUniformLocation(prog,"sel"),selArr);
  gl.uniform1i(gl.getUniformLocation(prog,"selCount"),Math.min(selected.size,16));
  const posLoc=gl.getAttribLocation(prog,"pos");
  gl.bindBuffer(gl.ARRAY_BUFFER,set.posBuf);
  gl.enableVertexAttribArray(posLoc);gl.vertexAttribPointer(posLoc,3,gl.FLOAT,false,0,0);
  const colLoc=gl.getAttribLocation(prog,"col");
  if(set.colBuf){gl.bindBuffer(gl.ARRAY_BUFFER,set.colBuf);gl.enableVertexAttribArray(colLoc);gl.vertexAttribPointer(colLoc,3,gl.UNSIGNED_BYTE,true,0,0);}
  else{gl.disableVertexAttribArray(colLoc);gl.vertexAttrib3f(colLoc,0.72,0.73,0.75);}
  const fidLoc=gl.getAttribLocation(prog,"fid");
  if(set.fidBuf){gl.bindBuffer(gl.ARRAY_BUFFER,set.fidBuf);gl.enableVertexAttribArray(fidLoc);gl.vertexAttribPointer(fidLoc,1,gl.FLOAT,false,0,0);}
  else{gl.disableVertexAttribArray(fidLoc);gl.vertexAttrib1f(fidLoc,-1);}
  gl.drawArrays(gl.TRIANGLES,0,set.count);
}

const leftPane=makePane("left",[{pos:seg.pos,col:null,fid:null}]);
const rightPane=makePane("right",[{pos:seg.pos,col:seg.col,fid:seg.fid},reb?{pos:reb.pos,col:reb.col,fid:reb.fid}:null,dev?{pos:dev.pos,col:dev.col,fid:dev.fid}:null].filter(Boolean));
const panes=[leftPane,rightPane].filter(Boolean);

function render(){for(const pane of panes)drawPane(pane,0);requestAnimationFrame(render);}
requestAnimationFrame(render);

const modeButton=document.getElementById("mode");
if(!reb)modeButton.style.display="none";
const MODES=dev
  ?[{tag:"Segmentation",next:"show rebuild"},
    {tag:"Sharp rebuild",next:"show deviation"},
    {tag:"Deviation vs scan",next:"show segmentation"}]
  :[{tag:"Segmentation",next:"show rebuild"},
    {tag:"Sharp rebuild",next:"show segmentation"}];
const devLegend=document.getElementById("devlegend");
modeButton.addEventListener("click",()=>{
  rightPane.active=(rightPane.active+1)%MODES.length;
  document.getElementById("rightTag").textContent=MODES[rightPane.active].tag;
  modeButton.textContent=MODES[rightPane.active].next;
  if(devLegend)devLegend.style.display=rightPane.active===2?"flex":"none";
});

const rows=document.getElementById("selRows");
const idsBox=document.getElementById("ids");
function refreshSelection(){
  rows.innerHTML="";
  for(const id of [...selected].sort((a,b)=>a-b)){
    const meta=META[id]||{label:"?",kind:"?",area:0,color:[128,128,128]};
    const row=document.createElement("div");row.className="row";
    const chip=document.createElement("i");chip.style.background=`rgb(${meta.color.join(",")})`;
    row.appendChild(chip);
    row.appendChild(document.createTextNode(`#${id} ${meta.label} (${meta.area.toFixed(0)} mm²)`));
    row.title="click to deselect";
    row.addEventListener("click",()=>{selected.delete(id);refreshSelection();});
    rows.appendChild(row);
  }
  idsBox.textContent=selected.size?[...selected].sort((a,b)=>a-b).join(", "):"none — click a coloured section";
}
document.getElementById("clear").addEventListener("click",()=>{selected.clear();refreshSelection();});

function pick(pane,clientX,clientY){
  const rect=pane.canvas.getBoundingClientRect();
  const dpr=window.devicePixelRatio||1;
  const x=Math.round((clientX-rect.left)*dpr);
  const y=Math.round((rect.bottom-clientY)*dpr);
  drawPane(pane,1);
  const gl=pane.gl;
  const px=new Uint8Array(4);
  gl.readPixels(x,y,1,1,gl.RGBA,gl.UNSIGNED_BYTE,px);
  drawPane(pane,0);
  const encoded=px[0]+(px[1]<<8)+(px[2]<<16);
  return encoded-2; // 0 background, 1 unowned, ids from 2 up
}

let dragging=false,panning=false,moved=0,lastX=0,lastY=0;
for(const pane of panes){
  const el=pane.canvas;
  el.addEventListener("pointerdown",e=>{dragging=true;moved=0;panning=e.shiftKey||e.button===2;lastX=e.clientX;lastY=e.clientY;el.setPointerCapture(e.pointerId);});
  el.addEventListener("pointerup",e=>{
    dragging=false;
    if(moved<4&&pane===rightPane){
      const featureId=pick(pane,e.clientX,e.clientY);
      if(featureId>=0){
        if(selected.has(featureId))selected.delete(featureId);else selected.add(featureId);
        refreshSelection();
      }
    }
  });
  el.addEventListener("pointermove",e=>{
    if(!dragging)return;
    const dx=e.clientX-lastX,dy=e.clientY-lastY;lastX=e.clientX;lastY=e.clientY;
    moved+=Math.abs(dx)+Math.abs(dy);
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
  el.addEventListener("wheel",e=>{e.preventDefault();cam.radius*=Math.exp(e.deltaY*0.0012);cam.radius=Math.min(DIAG*10,Math.max(DIAG*0.12,cam.radius));},{passive:false});
  el.addEventListener("contextmenu",e=>e.preventDefault());
}
refreshSelection();
</script></body></html>
"#;
