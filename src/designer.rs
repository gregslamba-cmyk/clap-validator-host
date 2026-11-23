pub fn generate_gui_designer_html(plugin_name: &str) -> String {
    let pn = plugin_name;
    let mut html = String::new();
    html.push_str(&format!("<div class='designer-tab' id='designer-{pn}'><h3>GUI Layout Designer - Drag & Drop</h3>"));
    html.push_str(&format!("<canvas id='canvas-{pn}' width='820' height='360' style='border:1px solid #444;background:#1a1d22'></canvas>"));
    html.push_str("<div class='designer-controls'>");
    html.push_str(&format!("<label>Snap: <select id='snap-{pn}'><option value='4'>4px</option><option value='8' selected>8px</option><option value='16'>16px</option></select></label>"));
    html.push_str(&format!("<label>Bg: <input type='color' id='bg-{pn}' value='#20232A'></label>"));
    html.push_str(&format!("<label>Accent: <input type='color' id='accent-{pn}' value='#E5A93B'></label>"));
    html.push_str(&format!("<button id='export-{pn}' style='background:#222;color:#fff;border:1px solid #555;padding:6px 12px;cursor:pointer'>Esporta / Copia gui_layout.json</button>"));
    html.push_str(&format!("<div id='status-{pn}' style='font-size:11px;color:#aaa;margin-top:6px'></div>"));
    html.push_str(&format!("<textarea id='json-{pn}' style='width:100%;height:120px;font-family:monospace;font-size:10px;display:none'></textarea>"));
    let js = r#"(function(){
var pn="__PN__";
var c=document.getElementById('canvas-'+pn);
var ctx=c.getContext('2d');
var snapSel=document.getElementById('snap-'+pn);
var bgIn=document.getElementById('bg-'+pn);
var accentIn=document.getElementById('accent-'+pn);
var exportBtn=document.getElementById('export-'+pn);
var status=document.getElementById('status-'+pn);
var jsonTa=document.getElementById('json-'+pn);
var snap=parseInt(snapSel.value,10);
var bgColor=bgIn.value;
var accentColor=accentIn.value;
var params=[{id:'threshold',label:'Threshold',type:'knob'},{id:'ratio',label:'Ratio',type:'knob'},{id:'attack',label:'Attack',type:'knob'},{id:'release',label:'Release',type:'knob'},{id:'makeup',label:'Makeup',type:'knob'},{id:'link',label:'Stereo Link',type:'toggle'},{id:'mode',label:'Mode',type:'enum'},{id:'in_meter',label:'IN Meter',type:'meter'},{id:'out_meter',label:'OUT Meter',type:'meter'},{id:'gr_meter',label:'GR Meter',type:'meter'}];
var widgets=[];
var dragging=null;
var resizing=null;
var offsetX=0,offsetY=0;
var gridW=80,gridH=88,perRow=6,startX=20,startY=30;
for(var i=0;i<params.length;i++){
 var p=params[i];
 var col=i%perRow;
 var row=Math.floor(i/perRow);
 widgets.push({id:p.id,label:p.label,type:p.type,x:Math.round((startX+col*gridW)/snap)*snap,y:Math.round((startY+row*gridH)/snap)*snap,w:gridW,h:gridH,selected:false});
}
function draw(){
 ctx.fillStyle=bgColor;
 ctx.fillRect(0,0,c.width,c.height);
 ctx.strokeStyle='#2a2d33';
 for(var gx=0;gx<c.width;gx+=snap){ctx.beginPath();ctx.moveTo(gx,0);ctx.lineTo(gx,c.height);ctx.stroke();}
 for(var gy=0;gy<c.height;gy+=snap){ctx.beginPath();ctx.moveTo(0,gy);ctx.lineTo(c.width,gy);ctx.stroke();}
 widgets.forEach(function(w){
  ctx.fillStyle='#2c2f36';
  ctx.fillRect(w.x,w.y,w.w,w.h);
  ctx.strokeStyle=accentColor;
  ctx.strokeRect(w.x,w.y,w.w,w.h);
  ctx.fillStyle='#cccccc';
  ctx.font='11px monospace';
  ctx.fillText(w.label,w.x+4,w.y+16);
  if(w.selected){
   ctx.fillStyle='#fff';
   ctx.fillRect(w.x+w.w-6,w.y+w.h-6,6,6);
   ctx.fillRect(w.x,w.y,6,6);
  }
 });
}
function getWidgetAt(x,y){
 for(var i=widgets.length-1;i>=0;i--){
  var w=widgets[i];
  if(x>=w.x&&x<w.x+w.w&&y>=w.y&&y<w.y+w.h)return w;
 }
 return null;
}
function hitResizeHandle(w,x,y){
 if(x>w.x+w.w-10&&y>w.y+w.h-10)return'se';
 return null;
}
c.addEventListener('mousedown',function(e){
 var rect=c.getBoundingClientRect();
 var x=e.clientX-rect.left;
 var y=e.clientY-rect.top;
 var w=getWidgetAt(x,y);
 if(!w){widgets.forEach(function(z){z.selected=false;});draw();return;}
 widgets.forEach(function(z){z.selected=false;});
 w.selected=true;
 var handle=hitResizeHandle(w,x,y);
 if(handle){
  resizing={widget:w,handle:handle,startX:x,startY:y,startW:w.w,startH:w.h};
 } else {
  dragging={widget:w,startX:x,startY:y,startW:w.w,startH:w.h};
  offsetX=x-w.x;
  offsetY=y-w.y;
 }
 draw();
});
c.addEventListener('mousemove',function(e){
 var rect=c.getBoundingClientRect();
 var x=e.clientX-rect.left;
 var y=e.clientY-rect.top;
 if(dragging){dragging.widget.x=Math.round((x-offsetX)/snap)*snap;dragging.widget.y=Math.round((y-offsetY)/snap)*snap;draw();} else if(resizing){
  resizing.widget.w=Math.max(40,Math.round((resizing.startW+(x-resizing.startX))/snap)*snap);
  resizing.widget.h=Math.max(30,Math.round((resizing.startH+(y-resizing.startY))/snap)*snap);
  draw();
 }
});
c.addEventListener('mouseup',function(){dragging=null;resizing=null;});
snapSel.addEventListener('change',function(){snap=parseInt(snapSel.value,10);draw();});
bgIn.addEventListener('input',function(){bgColor=bgIn.value;draw();});
accentIn.addEventListener('input',function(){accentColor=accentIn.value;draw();});
exportBtn.addEventListener('click',function(){
 var layout={window_width:c.width,window_height:c.height,background_hex:bgColor,accent_hex:accentColor,elements:widgets.map(function(w){return{param_id:w.id,label:w.label,widget_type:w.type,x:w.x,y:w.y,width:w.w,height:w.h};})};
 var jsonStr=JSON.stringify(layout,null,2);
 jsonTa.value=jsonStr;
 jsonTa.style.display='block';
 navigator.clipboard.writeText(jsonStr).then(function(){
  status.textContent='JSON copied to clipboard!';
  setTimeout(function(){status.textContent="";},2000);
 });
});
draw();
})();"#;
    let js = js.replace("__PN__", pn);
    html.push_str("<script>");
    html.push_str(&js);
    html.push_str("</script>");
    html.push_str("</div>");
    html
}
