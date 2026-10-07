// kestrel hello — the k API end to end. Host calls BLOCK the script:
// sequential automation reads exactly like it runs.
const site = k.args.site;
const page = k.open(site, { engine: 'cdp' });

k.log('title:', page.title());
k.log('h1:', page.text('h1'));
k.log('li.item count:', page.count('li.item'));

const links = page.extract('a');
k.log('links:', links.length, '| first:', JSON.stringify(links[0]));

const nav = page.goto(site + '/page2.html');
k.log('goto →', JSON.stringify(nav));

page.close();
