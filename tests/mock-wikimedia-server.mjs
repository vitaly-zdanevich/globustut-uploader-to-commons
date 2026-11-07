import { createServer } from 'node:http';

const port = Number(process.env.E2E_MOCK_PORT ?? '4174');
const expectedPassword = process.env.E2E_COMMONS_BOT_PASSWORD ?? 'mock Commons BotPassword';
const botUsername = 'Globustut@globustut-uploader';
const accountName = 'Globustut_users';
const loginToken = 'mock-login-token';
const csrfToken = 'mock-csrf-token';
const fileKey = 'mock-stashed-file';
const loginCookie = 'mock_commons_login=issued';
const sessionCookie = 'mock_commons_session=authenticated';

const nearbyBindings = Array.from({ length: 25 }, (_, index) => ({
  itemLabel: { value: `Test place ${index + 1}` },
  commonsCategory: { value: `Nearby category ${String(index + 1).padStart(2, '0')}` },
  distance: { value: ((25 - index) / 10).toFixed(1) },
}));

const state = {
  loginAttempts: 0,
  chunkUploads: 0,
  finalUploads: [],
  publishedTitles: new Set(),
};

const json = (response, payload, status = 200, headers = {}) => {
  response.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    ...headers,
  });
  response.end(JSON.stringify(payload));
};

const apiError = (response, code, info) => {
  json(response, { error: { code, info } });
};

const readBody = async (request) => {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  return Buffer.concat(chunks);
};

const parseMultipart = (body, contentType) => {
  const boundaryMatch = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType);
  if (!boundaryMatch) throw new Error('multipart boundary is missing');
  const boundary = boundaryMatch[1] ?? boundaryMatch[2].trim();
  const fields = new Map();
  const encoded = body.toString('latin1');
  for (const rawPart of encoded.split(`--${boundary}`)) {
    const separator = rawPart.indexOf('\r\n\r\n');
    if (separator < 0) continue;
    const headers = rawPart.slice(0, separator);
    const name = /name="([^"]+)"/i.exec(headers)?.[1];
    if (!name) continue;
    let rawValue = rawPart.slice(separator + 4);
    if (rawValue.endsWith('\r\n')) rawValue = rawValue.slice(0, -2);
    const filename = /filename="([^"]*)"/i.exec(headers)?.[1];
    if (filename !== undefined) {
      fields.set(name, {
        filename,
        size: Buffer.from(rawValue, 'latin1').length,
      });
    } else {
      fields.set(name, Buffer.from(rawValue, 'latin1').toString('utf8'));
    }
  }
  return fields;
};

const hasCookie = (request, cookie) => (
  (request.headers.cookie ?? '').split(';').some((value) => value.trim() === cookie)
);

const requireSession = (request, response) => {
  if (hasCookie(request, sessionCookie)) return true;
  apiError(response, 'assertuserfailed', 'mock Commons session is missing');
  return false;
};

const wikiFilename = (filename) => encodeURIComponent(filename.replaceAll(' ', '_'));

const handleApiGet = (request, response, url) => {
  const list = url.searchParams.get('list');
  if (list) {
    const hits = [
      { title: 'Category:Castles in Belarus' },
      { title: 'Category:Castles of Minsk' },
      { title: 'Category:Castles of Hrodna Region' },
      { title: 'Category:Castles of Viciebsk Region' },
      { title: 'Category:Castles of Mahilioŭ Region' },
    ];
    json(response, { query: { [list === 'search' ? 'search' : 'prefixsearch']: hits } });
    return;
  }

  const meta = url.searchParams.get('meta') ?? '';
  if (meta === 'tokens' && url.searchParams.get('type') === 'login') {
    json(
      response,
      { query: { tokens: { logintoken: loginToken } } },
      200,
      { 'Set-Cookie': `${loginCookie}; Path=/; HttpOnly; SameSite=Lax` },
    );
    return;
  }
  if (meta === 'tokens|userinfo' && url.searchParams.get('type') === 'csrf') {
    if (!requireSession(request, response)) return;
    json(response, {
      query: {
        tokens: { csrftoken: csrfToken },
        userinfo: { id: 1, name: accountName },
      },
    });
    return;
  }

  const title = url.searchParams.get('titles');
  if (title) {
    if (!requireSession(request, response)) return;
    const filename = title.replace(/^File:/, '');
    if (!state.publishedTitles.has(filename)) {
      json(response, { query: { pages: [{ title, missing: true }] } });
      return;
    }
    if (url.searchParams.get('prop') === 'imageinfo') {
      json(response, {
        query: {
          pages: [{
            title,
            imageinfo: [{
              descriptionurl: `https://commons.wikimedia.org/wiki/File:${wikiFilename(filename)}`,
              url: `https://upload.wikimedia.org/mock/${encodeURIComponent(filename)}`,
            }],
          }],
        },
      });
      return;
    }
    json(response, { query: { pages: [{ pageid: 1, title }] } });
    return;
  }

  apiError(response, 'mock-unsupported-query', 'unsupported mock MediaWiki GET request');
};

const handleApiPost = async (request, response) => {
  const body = await readBody(request);
  const contentType = request.headers['content-type'] ?? '';

  if (contentType.startsWith('application/x-www-form-urlencoded')) {
    const form = new URLSearchParams(body.toString('utf8'));
    if (form.get('action') !== 'login') {
      apiError(response, 'mock-unsupported-post', 'unsupported form request');
      return;
    }
    state.loginAttempts += 1;
    const valid = hasCookie(request, loginCookie)
      && form.get('lgname') === botUsername
      && form.get('lgpassword') === expectedPassword
      && form.get('lgtoken') === loginToken;
    if (!valid) {
      json(response, { login: { result: 'Failed', reason: 'invalid mock credentials' } });
      return;
    }
    json(
      response,
      { login: { result: 'Success', lgusername: accountName } },
      200,
      { 'Set-Cookie': `${sessionCookie}; Path=/; HttpOnly; SameSite=Lax` },
    );
    return;
  }

  if (!contentType.startsWith('multipart/form-data')) {
    apiError(response, 'mock-invalid-content-type', 'multipart/form-data is required');
    return;
  }
  if (!requireSession(request, response)) return;

  const fields = parseMultipart(body, contentType);
  const authenticated = fields.get('action') === 'upload'
    && fields.get('assert') === 'user'
    && fields.get('assertuser') === accountName
    && fields.get('token') === csrfToken;
  if (!authenticated) {
    apiError(response, 'badtoken', 'invalid mock upload authentication');
    return;
  }

  if (fields.get('stash') === '1') {
    state.chunkUploads += 1;
    const fileSize = Number(fields.get('filesize'));
    const offset = Number(fields.get('offset'));
    const chunk = fields.get('chunk');
    if (
      !Number.isSafeInteger(fileSize)
      || fileSize <= 0
      || !Number.isSafeInteger(offset)
      || offset < 0
      || typeof chunk !== 'object'
      || chunk.size <= 0
    ) {
      apiError(response, 'mock-invalid-chunk', 'invalid chunk metadata');
      return;
    }
    const nextOffset = Math.min(fileSize, offset + chunk.size);
    json(response, {
      upload: {
        result: 'Continue',
        offset: nextOffset,
        filekey: fileKey,
      },
    });
    return;
  }

  const filename = fields.get('filename');
  const text = fields.get('text');
  if (
    fields.get('filekey') !== fileKey
    || typeof filename !== 'string'
    || !filename
    || typeof text !== 'string'
    || !text
  ) {
    apiError(response, 'mock-invalid-final-upload', 'invalid final upload request');
    return;
  }
  state.publishedTitles.add(filename);
  state.finalUploads.push({
    filename,
    text,
    comment: fields.get('comment'),
  });
  json(response, {
    upload: {
      result: 'Success',
      filename,
      imageinfo: {
        descriptionurl: `https://commons.wikimedia.org/wiki/File:${wikiFilename(filename)}`,
        url: `https://upload.wikimedia.org/mock/${encodeURIComponent(filename)}`,
      },
    },
  });
};

const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url ?? '/', `http://127.0.0.1:${port}`);
    if (url.pathname === '/w/api.php' && request.method === 'GET') {
      handleApiGet(request, response, url);
      return;
    }
    if (url.pathname === '/w/api.php' && request.method === 'POST') {
      await handleApiPost(request, response);
      return;
    }
    if (url.pathname === '/sparql' && request.method === 'GET') {
      json(response, { results: { bindings: nearbyBindings } });
      return;
    }
    if (url.pathname === '/__mock/state' && request.method === 'GET') {
      json(response, {
        loginAttempts: state.loginAttempts,
        chunkUploads: state.chunkUploads,
        finalUploads: state.finalUploads,
        publishedTitles: [...state.publishedTitles],
      });
      return;
    }
    json(response, { error: 'not found' }, 404);
  } catch (error) {
    json(response, { error: String(error) }, 500);
  }
});

server.listen(port, '127.0.0.1');
process.on('SIGTERM', () => server.close());
process.on('SIGINT', () => server.close());
