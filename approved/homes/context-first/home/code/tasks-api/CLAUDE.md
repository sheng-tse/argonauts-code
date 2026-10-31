# Shop

A small web shop: a catalogue, a cart and a checkout, served by one Python app. This file is
what Claude Code reads first in this repository, so it says how the code is laid out and how
to work in it.

## Layout

- `shop/catalogue.py` lists the products, their prices and how many are in stock.
- `shop/cart.py` keeps each visitor's cart between requests, keyed by their session.
- `shop/checkout.py` takes a cart to an order, charges it and sends the receipt.
- `shop/receipts.py` writes the receipt as plain text and as HTML.
- `shop/web.py` holds the routes, one function for each page.
- `tests/` holds one test file for each module, named after it.

## Running it

- Install with `python -m pip install -e .` in a fresh virtual environment.
- Start it with `python -m shop` and open the address it prints.
- Run the tests with `python -m pytest` before you say a change is done.

## Working here

- Keep each function short, and name it by what it does.
- Prices are whole cents in an int, never a float.
- Stock never goes below zero: an order that would take it there is refused whole.
- A change to the checkout needs a test that walks one order through it.
- A new page gets a route in `shop/web.py` and a test that loads it.
- Don't add a dependency without saying why in the pull request.
- Write commit subjects in the imperative, under 72 characters.
- Keep the README's list of pages in step with the routes.

## Data

- The catalogue is read from `data/products.csv` when the app starts.
- Orders are kept in `data/orders.sqlite`, which the tests make fresh each time.
- Never commit a real order or a customer's address; the tests make up their own.

## When you are unsure

- Ask before you change how a price or a receipt is worked out.
- Ask before you remove a route, since links to it may be printed on receipts.
- Say which tests you ran, and which you could not.
- Say what you left undone, and why, so the next session can start there.
- Leave the code as tidy as you found it.
