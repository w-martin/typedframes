CREATE TABLE orders (
    order_id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL,
    amount NUMERIC(10, 2) NOT NULL,
    status TEXT NOT NULL
);

INSERT INTO orders (order_id, customer_id, amount, status) VALUES
    (1, 101, 150.00, 'completed'),
    (2, 102, 89.50, 'completed'),
    (3, 101, 220.10, 'pending');

CREATE TABLE products (
    product_id INTEGER PRIMARY KEY,
    price NUMERIC(10, 2) NOT NULL,
    stock INTEGER NOT NULL
);

INSERT INTO products (product_id, price, stock) VALUES
    (1, 19.99, 42),
    (2, 34.50, 17);
