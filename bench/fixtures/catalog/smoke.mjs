// Existing smoke test: the API is up and lists the stored products.
const response = await fetch(process.env.WEB_URL);
if (!response.ok) throw new Error(`HTTP ${response.status}`);
const products = await response.json();
if (!products.some((product) => product.sku === 'BOOK-1')) throw new Error('BOOK-1 missing');
console.log('smoke ok');
