const site = k.args.site;
const page = k.open(site, { engine: 'cdp' });
const shot = page.screenshot({ full: true });
k.save(k.args.out || 'shot.png', shot);
k.log('saved', shot.bytes, 'bytes');
page.close();
