// hybrid: lite HTTP first, browser only when the page needs JS
const res = k.fetch(k.args.site);
k.log('lite status:', res.status, '| bytes:', res.text.length);
const page = k.open(k.args.site, { engine: 'cdp' });
k.log('cdp title:', page.title());
page.close();
