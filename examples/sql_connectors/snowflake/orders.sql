SELECT order_id, customer_id, amount, status
FROM orders
WHERE status = 'completed'
