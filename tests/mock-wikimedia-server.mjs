import { createServer } from 'node:http';

const nearbyBindings = Array.from({ length: 25 }, (_, index) => ({
  itemLabel: { value: `Test place ${index + 1}` },
  commonsCategory: { value: `Nearby category ${String(index + 1).padStart(2, '0')}` },
  distance: { value: ((25 - index) / 10).toFixed(1) },
}));

const server = createServer((request, response) => {
  const url = new URL(request.url ?? '/', 'http://127.0.0.1:4174');
  response.setHeader('Content-Type', 'application/json');
  if (url.pathname === '/w/api.php') {
    const list = url.searchParams.get('list');
    const hits = [
      { title: 'Category:Castles in Belarus' },
      { title: 'Category:Castles of Minsk' },
      { title: 'Category:Castles of Hrodna Region' },
      { title: 'Category:Castles of Viciebsk Region' },
      { title: 'Category:Castles of Mahilioŭ Region' },
    ];
    response.end(JSON.stringify({ query: { [list === 'search' ? 'search' : 'prefixsearch']: hits } }));
    return;
  }
  if (url.pathname === '/sparql') {
    response.end(JSON.stringify({ results: { bindings: nearbyBindings } }));
    return;
  }
  response.statusCode = 404;
  response.end(JSON.stringify({ error: 'not found' }));
});

server.listen(4174, '127.0.0.1');
process.on('SIGTERM', () => server.close());
