#!/usr/bin/env python3
"""
Precisely fix tenant_id→aid in Rust handler files.
Only changes Rust variable/struct references, NOT SQL string column names.
"""

import re, glob

def fix_rust_var_refs(text):
    """Replace tenant_id Rust variable references while keeping SQL string column names"""
    
    # 1. let tenant_id = Uuid::parse_str(&claims.aid) → let aid = ...
    text = re.sub(
        r'(let\s+)tenant_id(\s*=\s*Uuid::parse_str\(\s*&\s*claims\.aid\s*\))',
        r'\1aid\2',
        text
    )
    
    # 2. let _tenant_id = Uuid::parse_str(&claims.aid) → let _aid = ...
    text = re.sub(
        r'(let\s+)_tenant_id(\s*=\s*Uuid::parse_str\(\s*&\s*claims\.aid\s*\))',
        r'\1_aid\2',
        text
    )
    
    # 3. .bind(tenant_id) → .bind(aid)
    text = re.sub(r'\.bind\(\s*tenant_id\s*\)', '.bind(aid)', text)
    text = re.sub(r'\.bind\(\s*&\s*tenant_id\s*\)', '.bind(&aid)', text)
    
    # 4. pub tenant_id: Uuid → pub aid: Uuid
    text = re.sub(r'\bpub\s+tenant_id\s*:\s*Uuid\b', 'pub aid: Uuid', text)
    
    # 5. tenant_id: Uuid (in struct fields without pub) → aid: Uuid
    # But NOT in SQL strings. This is tricky.
    text = re.sub(r'(?<!")\btenant_id\s*:\s*Uuid\b', 'aid: Uuid', text)
    
    # 6. features::enforce_feature_limit(&state.db, tenant_id → aid
    text = re.sub(
        r'(features::enforce_feature_limit\s*\(\s*&\s*state\.db\s*,\s*)tenant_id\b',
        r'\1aid',
        text
    )
    
    # 7. $1, tenant_id) or $1, tenant_id, → the variable ref (not in SQL)
    # Match: comma+space+tenant_id+comma or close-paren, but ensure it's not inside SQL
    # This is limited. Let me just handle the specific cases we know exist.
    # Actually let me find them by scanning the file for bare tenant_id refs.
    
    return text

def find_bare_refs(text, fname):
    """Find remaining tenant_id references that might need changing"""
    lines = text.split('\n')
    issues = []
    import re
    for i, line in enumerate(lines, 1):
        stripped = line.strip()
        # Skip comments
        if stripped.lstrip().startswith('//'):
            continue
        # Skip raw SQL strings
        if 'r#"' in stripped or '"#' in stripped:
            continue
        # Look for bare tenant_id (not let, not .bind, not pub, not :Uuid, not in string)
        # Focus on function call args
        if re.search(r'[,(]\s*tenant_id\b', stripped) and not re.search(r'r#"|"#|claims\.aid|let\s+tenant_id|:\s*Uuid', stripped):
            issues.append((i, stripped[:120]))
    return issues

if __name__ == '__main__':
    for fpath in sorted(glob.glob('src/handlers/*.rs')):
        basename = fpath.split('/')[-1]
        if basename == 'mod.rs' or basename == 'account_handler.rs' or basename == 'industry_handler.rs':
            continue
        
        with open(fpath) as f:
            original = f.read()
        
        fixed = fix_rust_var_refs(original)
        
        if fixed != original:
            with open(fpath, 'w') as f:
                f.write(fixed)
            print(f"✓ {basename}")
        else:
            # Check if there are bare refs that need manual handling
            refs = find_bare_refs(original, fpath)
            if refs:
                print(f"? {basename} ({len(refs)} possible refs)")
                for line_no, content in refs[:3]:
                    print(f"    L{line_no}: {content}")
            else:
                print(f"  {basename}: clean")
