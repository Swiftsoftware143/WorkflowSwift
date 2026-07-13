#!/usr/bin/env python3
"""Precise fix of Rust variable references to tenant_id -> aid, excluding SQL strings and JSON keys."""

import re, sys

def fix_file(fpath):
    with open(fpath) as f:
        text = f.read()
        lines = text.split('\n')
    
    output = []
    changes = 0
    
    for line in lines:
        stripped = line.strip()
        
        # Skip comments
        if stripped.lstrip().startswith('//'):
            output.append(line)
            continue
        
        # Skip obvious SQL strings
        if 'r#"' in line or '"#' in line:
            output.append(line)
            continue
        
        # For lines inside r#"..."#, skip entirely
        # We'll handle by detecting SQL boundaries
        
        new_line = line
        
        # Pattern 1: features::enforce_feature_limit(&state.db, tenant_id, ...
        # The tenant_id here is a variable reference (the variable was renamed to aid)
        # But only if it's AFTER a `let aid = ...` line
        # Actually, the sed already changed let tenant_id -> let aid, so these are stale refs
        if 'features::enforce_feature_limit' in line and 'tenant_id,' in line:
            new_line = new_line.replace('tenant_id,', 'aid,')
            if new_line != line:
                changes += 1
        
        # Pattern 2: function calls passing tenant_id (not in SQL strings)
        # e.g. forward_dispatch(&state.db, target_id, tenant_id, &payload)
        # e.g. convert_steps_to_n8n(&step_values, tenant_id, id, &callback_base_url)
        if 'tenant_id,' in line and not 'claims.aid' in line:
            # Only replace if not in a SQL string context
            if not is_in_sql_string(line):
                new_line = new_line.replace('tenant_id,', 'aid,')
                if new_line != line:
                    changes += 1
        
        # Pattern 3: variable references like tenant_id.to_string() 
        if 'tenant_id.' in line and not 'claims.aid' in line:
            new_line = new_line.replace('tenant_id.', 'aid.')
            if new_line != line:
                changes += 1
        
        # Pattern 4: let tid = tenant_id;
        if 'tenant_id;' in line:
            new_line = new_line.replace('tenant_id;', 'aid;')
            if new_line != line:
                changes += 1
        
        output.append(new_line)
    
    if changes > 0:
        with open(fpath, 'w') as f:
            f.write('\n'.join(output))
        print(f"  {fpath}: {changes} change(s)")
    return changes

def is_in_sql_string(line):
    """Heuristic: if line contains raw SQL markers, skip"""
    if 'r#"' in line or '"#' in line:
        return True
    return False

# Also handle the specific complex cases
def fix_complex_cases(fpath):
    with open(fpath) as f:
        text = f.read()
    
    changes = 0
    
    # incoming_handler: let tenant_id = workflow.tenant_id -> let aid = workflow.aid
    text, n = re.subn(r'let tenant_id = workflow\.tenant_id', 'let aid = workflow.aid', text)
    changes += n
    
    # incoming_handler: tenant_id,  (function arg)
    text, n = re.subn(r'(\b)tenant_id(\s*,\s*)', r'\1aid\2', text)
    
    # plan_handler: let tenant_id = req.get("tenant_id") -> stays (JSON key)
    # But the second reference: .ok_or_else(|| "Valid tenant_id is required") -> stays
    
    # portfolio_handler: let tenant_id = body.get("tenant_id") -> stays (JSON key)
    
    # internal_handler: "tenant_id": tenant_id.to_string()
    text, n = re.subn(r'"tenant_id": tenant_id\.to_string', '"tenant_id": aid.to_string', text)
    changes += n
    
    # provider_keys_handler: let tenant_id_str = tenant_id.to_string()
    text, n = re.subn(r'let tenant_id_str = tenant_id\.to_string', 'let tenant_id_str = aid.to_string', text)
    changes += n
    # Also references to tenant_id_str should stay (it's already named with tenant_id prefix)
    
    # user_integration_handler: let tid = tenant_id;
    text, n = re.subn(r'let tid = tenant_id;', 'let tid = aid;', text)
    changes += n
    
    with open(fpath, 'w') as f:
        f.write(text)
    
    if changes > 0:
        print(f"  {fpath}: {changes} complex change(s)")
    return changes

if __name__ == '__main__':
    import glob
    total = 0
    for fpath in sorted(glob.glob('src/handlers/*.rs')):
        total += fix_file(fpath)
        total += fix_complex_cases(fpath)
    print(f"\nTotal changes: {total}")
