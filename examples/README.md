# Examples

Quick start examples for Bijection.

## Files

- `customers_v1.csv` - Original customer data
- `customers_v2.csv` - Updated customer data (added columns, renamed field, modified values)
- `schema-policy.json` - Sample schema contract/policy
- `batch-manifest.json` - Sample batch manifest for multi-pair comparison

## Try It Out

From the repo root:

### Schema Comparison

```bash
biject schema \
  --source examples/customers_v1.csv \
  --target examples/customers_v2.csv
```

### Schema with Policy Validation

```bash
biject schema \
  --source examples/customers_v1.csv \
  --target examples/customers_v2.csv \
  --policy examples/schema-policy.json
```

### Data Diff

```bash
biject data \
  --source examples/customers_v1.csv \
  --target examples/customers_v2.csv \
  --key customer_id
```

### Data Diff with Stats Only

```bash
biject data \
  --source examples/customers_v1.csv \
  --target examples/customers_v2.csv \
  --key customer_id \
  --diffs-only
```

### Batch Comparison

```bash
biject batch \
  --manifest examples/batch-manifest.json \
  --key customer_id
```

## What to Expect

**Schema Diff:**
- Detects added columns: `country`, `last_purchase_date`
- Detects removed columns: (none in this case)
- Identifies type changes in shared columns (likely none, all string/numeric)
- Notes column safety classification

**Data Diff:**
- v1 row 5 (Eve Davis) is removed in v2
- v2 row 6 (Frank Miller) is new
- Rows 1-4 are present in both but may have modified values (lifetime_value, name variant)

**Batch:**
- Single pair result showing summary of changes across the pair
